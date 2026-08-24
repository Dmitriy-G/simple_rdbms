use catalog::{Catalog, Column, Schema};
use common::{DbConfig, Error, Result, TxnId};
use executor::ExecutorContext;
use planner::{Binder, BoundStatement, PhysicalPlan, to_physical};
use sql::{Lexer, Parser, SqlError, Statement};
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

const REPLACER_K: usize = 2;

#[derive(Debug, Clone, Copy)]
enum TxnSlot {
    None,
    Active(TxnId),
    Aborted(TxnId),
}

impl TxnSlot {
    fn txn_id(self) -> Option<TxnId> {
        match self {
            TxnSlot::None => None,
            TxnSlot::Active(txn_id) | TxnSlot::Aborted(txn_id) => Some(txn_id),
        }
    }
}

pub struct Database {
    catalog: Catalog,
    buffer_pool: BufferPool,
    txn_manager: TransactionManager,
    checkpoint_byte_threshold: u64,
    bytes_at_last_checkpoint: u64,
    txn_slot: TxnSlot,
}

impl Database {
    pub fn open(config: DbConfig) -> Result<Self> {
        Self::open_impl(config).inspect_err(|err| {
            tracing::error!(sql_state = %err.sql_state(), %err, "failed to open database");
        })
    }

    fn open_impl(config: DbConfig) -> Result<Self> {
        let mut wal_path = config.db_path.clone().into_os_string();
        wal_path.push(".wal");
        let log_manager = LogManager::open(wal_path)?;
        let disk_manager = DiskManager::open(config.db_path.clone(), config.page_size)?;
        let mut dwb_path = config.db_path.clone().into_os_string();
        dwb_path.push(".dwb");
        let dwb = DoubleWriteBuffer::open(dwb_path, config.dwb_capacity)?;
        Self::open_with_managers(config, disk_manager, dwb, log_manager)
    }

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
            txn_slot: TxnSlot::None,
        })
    }

    pub fn close(mut self) -> Result<()> {
        write_checkpoint(&self.buffer_pool, &mut self.txn_manager)?;
        self.buffer_pool.flush_log_all()?;
        self.buffer_pool.flush_all()?;
        self.buffer_pool.sync()?;
        Ok(())
    }

    fn maybe_checkpoint(&mut self) -> Result<()> {
        let grown = self.buffer_pool.log_bytes_appended() - self.bytes_at_last_checkpoint;
        if grown >= self.checkpoint_byte_threshold {
            write_checkpoint(&self.buffer_pool, &mut self.txn_manager)?;
            self.bytes_at_last_checkpoint = self.buffer_pool.log_bytes_appended();
        }
        Ok(())
    }

    pub fn execute(&mut self, sql: &str) -> Result<ResultSet> {
        self.execute_impl(sql).inspect_err(|err| {
            tracing::error!(sql_state = %err.sql_state(), %err, "statement failed");
        })
    }

    fn execute_impl(&mut self, sql: &str) -> Result<ResultSet> {
        let tokens = Lexer::new(sql).tokenize().map_err(|err| syntax_error(&err, sql))?;
        let statement = Parser::new(tokens).parse().map_err(|err| syntax_error(&err, sql))?;

        match &statement {
            Statement::Begin => return self.handle_begin(),
            Statement::Commit => return self.handle_commit(),
            Statement::Rollback => return self.handle_rollback(),
            _ => {}
        }

        let (txn_id, autocommit) = self.txn_for_statement()?;
        let bound = Binder::new(&self.catalog).bind(statement).map_err(Error::from);
        let result = bound.and_then(|bound| self.execute_bound(bound, txn_id));

        if autocommit {
            match &result {
                Ok(_) => {
                    self.txn_manager.commit(txn_id, &self.buffer_pool)?;
                    self.maybe_checkpoint()?;
                }
                Err(_) => {
                    let _ = self.txn_manager.abort(txn_id, &self.buffer_pool);
                }
            }
        } else if result.is_err() {
            self.txn_slot = TxnSlot::Aborted(txn_id);
        }
        result
    }

    fn handle_begin(&mut self) -> Result<ResultSet> {
        if self.txn_slot.txn_id().is_some() {
            return Err(Error::NestedTransaction);
        }
        let txn_id = self.txn_manager.begin(&self.buffer_pool, IsolationLevel::ReadCommitted)?;
        self.txn_slot = TxnSlot::Active(txn_id);
        Ok(ResultSet::rows_affected(0))
    }

    fn handle_commit(&mut self) -> Result<ResultSet> {
        match self.txn_slot {
            TxnSlot::None => Err(Error::NoActiveTransaction { statement: "COMMIT".to_string() }),
            TxnSlot::Aborted(txn_id) => {
                self.txn_manager.abort(txn_id, &self.buffer_pool)?;
                self.txn_slot = TxnSlot::None;
                self.maybe_checkpoint()?;
                Ok(ResultSet::RolledBack)
            }
            TxnSlot::Active(txn_id) => {
                self.txn_manager.commit(txn_id, &self.buffer_pool)?;
                self.txn_slot = TxnSlot::None;
                self.maybe_checkpoint()?;
                Ok(ResultSet::rows_affected(0))
            }
        }
    }

    fn handle_rollback(&mut self) -> Result<ResultSet> {
        let txn_id = self
            .txn_slot
            .txn_id()
            .ok_or_else(|| Error::NoActiveTransaction { statement: "ROLLBACK".to_string() })?;
        self.txn_manager.abort(txn_id, &self.buffer_pool)?;

        let reload_txn =
            self.txn_manager.begin(&self.buffer_pool, IsolationLevel::ReadCommitted)?;
        self.catalog = Catalog::open(&self.buffer_pool, reload_txn)?;
        self.txn_manager.commit(reload_txn, &self.buffer_pool)?;

        self.txn_slot = TxnSlot::None;
        self.maybe_checkpoint()?;
        Ok(ResultSet::rows_affected(0))
    }

    fn txn_for_statement(&mut self) -> Result<(TxnId, bool)> {
        match self.txn_slot {
            TxnSlot::Active(txn_id) => Ok((txn_id, false)),
            TxnSlot::Aborted(_) => Err(Error::TransactionAborted),
            TxnSlot::None => {
                let txn_id =
                    self.txn_manager.begin(&self.buffer_pool, IsolationLevel::ReadCommitted)?;
                Ok((txn_id, true))
            }
        }
    }

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

    pub fn table_names(&self) -> Vec<String> {
        self.catalog.table_names().into_iter().map(String::from).collect()
    }

    pub fn table_schema(&self, name: &str) -> Result<Schema> {
        Ok(self.catalog.get_table(name)?.schema.clone())
    }

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
    fn drop(&mut self) {
        let _ = self.buffer_pool.flush_log_all();
        let _ = self.buffer_pool.flush_all();
    }
}

fn syntax_error(err: &SqlError, sql: &str) -> Error {
    Error::Syntax { message: err.render(sql), offset: err.offset(sql) }
}
