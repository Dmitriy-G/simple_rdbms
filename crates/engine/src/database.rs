use catalog::{Catalog, Column, Schema};
use common::{DbConfig, Error, Result, TxnId};
use executor::ExecutorContext;
use planner::{Binder, BoundStatement, PhysicalPlan, to_physical};
use sql::{Lexer, Parser, Statement};
use storage::buffer::BufferPool;
use storage::disk::DiskManager;
use storage::dwb::DoubleWriteBuffer;
use storage::recovery;
use storage::replacer::LruKReplacer;
use storage::wal::LogManager;
use txn::{IsolationLevel, TransactionManager, write_checkpoint};
use types::{Tuple, Value};

use crate::executor_factory::build_executor;
use crate::result_set::ResultSet;

/// The number of recent accesses the buffer pool's `LruKReplacer` tracks per
/// frame before falling back to plain LRU. Not yet exposed as a config knob;
/// `2` is the classic LRU-2 choice.
const REPLACER_K: usize = 2;

/// A single open database. Owns the catalog, buffer pool, and transaction
/// manager, and is the object every SQL statement is executed against.
pub struct Database {
    catalog: Catalog,
    buffer_pool: BufferPool,
    txn_manager: TransactionManager,
    /// The trigger for `write_checkpoint`: once `buffer_pool.log_bytes_appended()`
    /// has grown past this checkpoint threshold beyond the value recorded here.
    checkpoint_byte_threshold: u64,
    /// `buffer_pool.log_bytes_appended()`'s value as of the last checkpoint
    /// (or database open, if none has happened yet this session).
    bytes_at_last_checkpoint: u64,
    /// The id of the explicit transaction opened by an unmatched `BEGIN`,
    /// if any. `None` means every statement runs autocommit: begin,
    /// execute, commit, all within `execute` itself. `Some` means the next
    /// statement joins this transaction instead, and only `COMMIT`/
    /// `ROLLBACK` end it.
    active_txn: Option<TxnId>,
}

impl Database {
    /// Opens (creating if necessary) the database described by `config`,
    /// standing up its disk manager and buffer pool, running crash recovery
    /// against whatever the write-ahead log describes, and only then
    /// loading the catalog - the pages the catalog reads must already be
    /// consistent before anything above `storage` starts trusting them.
    pub fn open(config: DbConfig) -> Result<Self> {
        let mut wal_path = config.db_path.clone().into_os_string();
        wal_path.push(".wal");
        let log_manager = LogManager::open(wal_path)?;
        let disk_manager = DiskManager::open(config.db_path.clone(), config.page_size)?;
        let mut dwb_path = config.db_path.clone().into_os_string();
        dwb_path.push(".dwb");
        let dwb = DoubleWriteBuffer::open(dwb_path, config.dwb_capacity)?;
        Self::open_with_managers(config, disk_manager, dwb, log_manager)
    }

    /// Opens a database backed by caller-supplied `BlockDevice`s rather than
    /// real files, for the crash-injection harness: wrapping a device in a
    /// `storage::block_device::FaultyDevice` lets a test simulate a crash at
    /// an exact point in a workload without actually killing a process.
    #[cfg(any(test, feature = "test-util"))]
    pub fn open_with_devices(
        config: DbConfig,
        db_device: Box<dyn storage::block_device::BlockDevice>,
        wal_device: Box<dyn storage::block_device::BlockDevice>,
        dwb_device: Box<dyn storage::block_device::BlockDevice>,
    ) -> Result<Self> {
        let disk_manager = DiskManager::open_with_device(db_device, config.page_size, None)?;
        let log_manager = LogManager::open_with_device(wal_device)?;
        let dwb = DoubleWriteBuffer::open_with_device(dwb_device, config.dwb_capacity)?;
        Self::open_with_managers(config, disk_manager, dwb, log_manager)
    }

    /// Shared tail of `open`/`open_with_devices`: repairs any real-file
    /// page a crash tore mid-write from the double-write buffer (must run
    /// before recovery trusts page contents, and before any `BufferPool`
    /// exists to run it against), stands up the buffer pool over the
    /// already-constructed managers, runs ARIES recovery, and loads the
    /// catalog.
    fn open_with_managers(
        config: DbConfig,
        mut disk_manager: DiskManager,
        mut dwb: DoubleWriteBuffer,
        log_manager: LogManager,
    ) -> Result<Self> {
        recovery::recover_double_write(&mut disk_manager, &mut dwb)?;

        let replacer = Box::new(LruKReplacer::new(config.buffer_pool_size, REPLACER_K));
        let buffer_pool =
            BufferPool::new(disk_manager, dwb, log_manager, config.buffer_pool_size, replacer);

        let highest_txn_id = recovery::recover(&buffer_pool)?;

        let mut txn_manager = TransactionManager::new(highest_txn_id);
        // The catalog's own bootstrap heap allocation (only ever happens on
        // a brand-new database) needs a real, committed transaction of its
        // own - otherwise it would be a dangling, never-committed write
        // that the next recovery would undo as a loser.
        let bootstrap_txn = txn_manager.begin(&buffer_pool, IsolationLevel::ReadCommitted)?;
        let catalog = Catalog::open(&buffer_pool, bootstrap_txn)?;
        txn_manager.commit(bootstrap_txn, &buffer_pool)?;

        let bytes_at_last_checkpoint = buffer_pool.log_bytes_appended();
        Ok(Self {
            catalog,
            buffer_pool,
            txn_manager,
            checkpoint_byte_threshold: config.checkpoint_byte_threshold,
            bytes_at_last_checkpoint,
            active_txn: None,
        })
    }

    /// Closes the database: writes a final checkpoint, forces every
    /// write-ahead log record to disk, flushes every dirty page, and syncs
    /// the file to durable storage. Errors here are real failures worth
    /// reporting, which is why this is a separate, explicit call rather
    /// than left entirely to `Drop` (which cannot report them).
    pub fn close(mut self) -> Result<()> {
        write_checkpoint(&self.buffer_pool, &mut self.txn_manager)?;
        self.buffer_pool.flush_log_all()?;
        self.buffer_pool.flush_all()?;
        self.buffer_pool.sync()?;
        Ok(())
    }

    /// Checkpoints if the log has grown by `checkpoint_byte_threshold`
    /// bytes since the last one, bounding how much of the log a future
    /// recovery has to scan. Called after every statement's commit.
    fn maybe_checkpoint(&mut self) -> Result<()> {
        let grown = self.buffer_pool.log_bytes_appended() - self.bytes_at_last_checkpoint;
        if grown >= self.checkpoint_byte_threshold {
            write_checkpoint(&self.buffer_pool, &mut self.txn_manager)?;
            self.bytes_at_last_checkpoint = self.buffer_pool.log_bytes_appended();
        }
        Ok(())
    }

    /// Parses, binds, plans, and executes one SQL statement, returning its
    /// result set. `BEGIN`/`START TRANSACTION`, `COMMIT`, and `ROLLBACK`
    /// are handled directly here, before binding - they reference no
    /// tables or columns, so binding is meaningless for them. Every other
    /// statement runs under `active_txn` if one is open (joining an
    /// explicit transaction), or under a fresh transaction begun and
    /// committed (or, on failure, aborted) right here otherwise
    /// (autocommit).
    pub fn execute(&mut self, sql: &str) -> Result<ResultSet> {
        let tokens = Lexer::new(sql).tokenize().map_err(|err| Error::Parse(err.render(sql)))?;
        let statement = Parser::new(tokens).parse().map_err(|err| Error::Parse(err.render(sql)))?;

        match &statement {
            Statement::Begin => return self.handle_begin(),
            Statement::Commit => return self.handle_commit(),
            Statement::Rollback => return self.handle_rollback(),
            _ => {}
        }

        let bound = Binder::new(&self.catalog).bind(statement)?;
        let (txn_id, autocommit) = self.txn_for_statement()?;
        let result = self.execute_bound(bound, txn_id);

        if autocommit {
            match &result {
                Ok(_) => {
                    self.txn_manager.commit(txn_id, &self.buffer_pool)?;
                    self.maybe_checkpoint()?;
                }
                Err(_) => {
                    // The statement failed partway through its own,
                    // just-begun transaction: undo whatever it managed to
                    // write rather than leaving it dangling for a future
                    // crash recovery to clean up. If the abort itself
                    // fails there's nothing more useful to report than the
                    // original error, so that's swallowed.
                    let _ = self.txn_manager.abort(txn_id, &self.buffer_pool);
                }
            }
        }
        result
    }

    /// `BEGIN`/`START TRANSACTION`: opens an explicit transaction. Nesting
    /// is an error.
    fn handle_begin(&mut self) -> Result<ResultSet> {
        if self.active_txn.is_some() {
            return Err(Error::Transaction(
                "already inside a transaction; nested BEGIN is not allowed".to_string(),
            ));
        }
        let txn_id = self.txn_manager.begin(&self.buffer_pool, IsolationLevel::ReadCommitted)?;
        self.active_txn = Some(txn_id);
        Ok(ResultSet::rows_affected(0))
    }

    /// `COMMIT`: ends the active explicit transaction, keeping its writes.
    /// An error with no active transaction.
    fn handle_commit(&mut self) -> Result<ResultSet> {
        let txn_id = self
            .active_txn
            .take()
            .ok_or_else(|| Error::Transaction("no active transaction to COMMIT".to_string()))?;
        self.txn_manager.commit(txn_id, &self.buffer_pool)?;
        self.maybe_checkpoint()?;
        Ok(ResultSet::rows_affected(0))
    }

    /// `ROLLBACK`: ends the active explicit transaction, undoing its
    /// writes, and reloads the catalog from its heap. An error with no
    /// active transaction.
    ///
    /// The reload is needed because a `CREATE TABLE` inside the aborted
    /// transaction has its heap-page writes undone by `abort` but the
    /// in-memory `tables_by_name`/`tables_by_id` maps don't know that;
    /// rather than trying to reverse them surgically, `Catalog::open`
    /// (already able to rebuild them from an existing heap) is simply
    /// called again, discarding the stale `Catalog` for one read fresh off
    /// disk - simplest thing that is actually correct.
    fn handle_rollback(&mut self) -> Result<ResultSet> {
        let txn_id = self
            .active_txn
            .take()
            .ok_or_else(|| Error::Transaction("no active transaction to ROLLBACK".to_string()))?;
        self.txn_manager.abort(txn_id, &self.buffer_pool)?;

        let reload_txn =
            self.txn_manager.begin(&self.buffer_pool, IsolationLevel::ReadCommitted)?;
        self.catalog = Catalog::open(&self.buffer_pool, reload_txn)?;
        self.txn_manager.commit(reload_txn, &self.buffer_pool)?;

        self.maybe_checkpoint()?;
        Ok(ResultSet::rows_affected(0))
    }

    /// The transaction the next statement should run under: `active_txn`
    /// if an explicit transaction is open (`autocommit = false`), or a
    /// freshly begun one otherwise (`autocommit = true`, meaning `execute`
    /// itself must commit or abort it once the statement is done).
    fn txn_for_statement(&mut self) -> Result<(TxnId, bool)> {
        match self.active_txn {
            Some(txn_id) => Ok((txn_id, false)),
            None => {
                let txn_id =
                    self.txn_manager.begin(&self.buffer_pool, IsolationLevel::ReadCommitted)?;
                Ok((txn_id, true))
            }
        }
    }

    /// Runs a bound, non-transaction-control statement under `txn_id`.
    fn execute_bound(&mut self, bound: BoundStatement, txn_id: TxnId) -> Result<ResultSet> {
        match bound {
            BoundStatement::CreateTable(create) => {
                let schema = Schema::new(
                    create
                        .columns
                        .into_iter()
                        .map(|column| Column::new(column.name, column.data_type, column.nullable))
                        .collect(),
                );
                self.catalog.create_table(&self.buffer_pool, txn_id, &create.table_name, schema)?;
                Ok(ResultSet::rows_affected(0))
            }
            BoundStatement::Insert(insert) => {
                let physical = to_physical(planner::plan(BoundStatement::Insert(insert))?);
                let rows = self.run(physical, txn_id)?;
                let inserted = rows
                    .first()
                    .and_then(|tuple| tuple.values().first())
                    .and_then(|value| match value {
                        Value::BigInt(count) => Some(*count as usize),
                        _ => None,
                    })
                    .unwrap_or(0);
                Ok(ResultSet::rows_affected(inserted))
            }
            BoundStatement::Select(select) => {
                let column_names = select.column_names.clone();
                let physical = to_physical(planner::plan(BoundStatement::Select(select))?);
                let rows = self.run(physical, txn_id)?;
                Ok(ResultSet::rows(column_names, rows))
            }
        }
    }

    /// Every registered table's name, sorted for a deterministic order.
    pub fn table_names(&self) -> Vec<String> {
        self.catalog.table_names().into_iter().map(String::from).collect()
    }

    /// The column schema of the table named `name`.
    pub fn table_schema(&self, name: &str) -> Result<Schema> {
        Ok(self.catalog.get_table(name)?.schema.clone())
    }

    /// Builds and drains an executor tree for `physical` under `txn_id`,
    /// collecting every tuple it produces.
    fn run(&self, physical: PhysicalPlan, txn_id: TxnId) -> Result<Vec<Tuple>> {
        let txn = self.txn_manager.get(txn_id)?;
        let mut executor = build_executor(physical);
        let mut ctx = ExecutorContext::new(&self.catalog, &self.buffer_pool, txn);
        executor.init(&mut ctx)?;

        let mut rows = Vec::new();
        while let Some(tuple) = executor.next(&mut ctx)? {
            rows.push(tuple);
        }
        Ok(rows)
    }
}

impl Drop for Database {
    /// Best-effort flush on drop, for callers that don't call `close`
    /// explicitly. Errors are swallowed here (there is nowhere to report
    /// them from `Drop`); call `close` to observe them.
    fn drop(&mut self) {
        let _ = self.buffer_pool.flush_log_all();
        let _ = self.buffer_pool.flush_all();
    }
}
