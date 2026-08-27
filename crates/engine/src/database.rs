use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use catalog::{Catalog, Column, Schema};
use common::sync::recover_lock;
use common::{DbConfig, Error, Result, Severity, TxnId};
use executor::ExecutorContext;
use planner::{
    Binder, BoundStatement, IndexScanRule, Optimizer, PhysicalPlan, explain_logical,
    explain_physical, to_physical,
};
use sql::{Lexer, Parser, SqlError, Statement};
use storage::StorageError;
use storage::btree::BTreeIndex;
use storage::buffer::BufferPool;
use storage::disk::DiskManager;
use storage::dwb::DoubleWriteBuffer;
use storage::heap::TableHeap;
use storage::recovery;
use storage::replacer::LruKReplacer;
use storage::wal::LogManager;
use txn::{IsolationLevel, TransactionManager, write_checkpoint};
use types::{MemcomparableEncode, Tuple, Value};

use crate::executor_factory::build_executor;
use crate::result_set::ResultSet;

const REPLACER_K: usize = 2;

static NEXT_CONN_ID: AtomicU64 = AtomicU64::new(1);

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
    buffer_pool: Arc<BufferPool>,
    txn_manager: Arc<Mutex<TransactionManager>>,
    checkpoint_byte_threshold: u64,
    bytes_at_last_checkpoint: u64,
    slow_query_warn_threshold_ms: u64,
    txn_slot: TxnSlot,
    txn_span: Option<tracing::Span>,
    next_stmt_id: u64,
    _conn_span: tracing::span::EnteredSpan,
}

impl Database {
    pub fn open(config: DbConfig) -> Result<Self> {
        Self::open_impl(config).inspect_err(|err| {
            tracing::error!(sql_state = %err.sql_state(), %err, "failed to open database");
        })
    }

    fn open_impl(config: DbConfig) -> Result<Self> {
        let conn_span = new_connection_span();
        let mut wal_path = config.db_path.clone().into_os_string();
        wal_path.push(".wal");
        let log_manager = LogManager::open(wal_path)?;
        let disk_manager = DiskManager::open(config.db_path.clone(), config.page_size)?;
        let mut dwb_path = config.db_path.clone().into_os_string();
        dwb_path.push(".dwb");
        let dwb = DoubleWriteBuffer::open(dwb_path, config.dwb_capacity)?;
        Self::open_with_managers(config, disk_manager, dwb, log_manager, conn_span)
    }

    #[cfg(any(test, feature = "test-util"))]
    pub fn open_with_devices(
        config: DbConfig,
        db_device: Box<dyn storage::block_device::BlockDevice>,
        wal_device: Box<dyn storage::block_device::BlockDevice>,
        dwb_device: Box<dyn storage::block_device::BlockDevice>,
    ) -> Result<Self> {
        let conn_span = new_connection_span();
        let disk_manager = DiskManager::open_with_device(db_device, config.page_size, None)?;
        let log_manager = LogManager::open_with_device(wal_device)?;
        let dwb = DoubleWriteBuffer::open_with_device(dwb_device, config.dwb_capacity)?;
        Self::open_with_managers(config, disk_manager, dwb, log_manager, conn_span)
    }

    fn open_with_managers(
        config: DbConfig,
        disk_manager: DiskManager,
        dwb: DoubleWriteBuffer,
        log_manager: LogManager,
        conn_span: tracing::span::EnteredSpan,
    ) -> Result<Self> {
        recovery::recover_double_write(&disk_manager, &dwb)?;

        let replacer = Box::new(LruKReplacer::new(config.buffer_pool_size, REPLACER_K));
        let buffer_pool = Arc::new(BufferPool::new(
            disk_manager,
            dwb,
            log_manager,
            config.buffer_pool_size,
            replacer,
        ));

        let highest_txn_id = recovery::recover(&buffer_pool)?;

        let mut txn_manager = TransactionManager::new(highest_txn_id);
        let bootstrap_txn = txn_manager.begin(&buffer_pool, IsolationLevel::ReadCommitted)?;
        let catalog = Catalog::open(&buffer_pool, bootstrap_txn)?;
        txn_manager.commit(bootstrap_txn, &buffer_pool)?;
        let txn_manager = Arc::new(Mutex::new(txn_manager));

        let bytes_at_last_checkpoint = buffer_pool.log_bytes_appended();

        tracing::info!(
            page_size = config.page_size,
            buffer_pool_frames = config.buffer_pool_size,
            dwb_capacity = config.dwb_capacity,
            checksums_enabled = true,
            recovered_highest_txn_id = ?highest_txn_id,
            "database opened"
        );

        Ok(Self {
            catalog,
            buffer_pool,
            txn_manager,
            checkpoint_byte_threshold: config.checkpoint_byte_threshold,
            bytes_at_last_checkpoint,
            slow_query_warn_threshold_ms: config.slow_query_warn_threshold_ms,
            txn_slot: TxnSlot::None,
            txn_span: None,
            next_stmt_id: 1,
            _conn_span: conn_span,
        })
    }

    pub fn close(self) -> Result<()> {
        let mut txn_manager = recover_lock(self.txn_manager.lock(), "Database.txn_manager");
        write_checkpoint(&self.buffer_pool, &mut txn_manager)?;
        drop(txn_manager);
        self.buffer_pool.flush_log_all()?;
        self.buffer_pool.flush_all()?;
        self.buffer_pool.sync()?;
        tracing::info!("connection closed");
        Ok(())
    }

    fn maybe_checkpoint(&mut self) -> Result<()> {
        let grown = self.buffer_pool.log_bytes_appended() - self.bytes_at_last_checkpoint;
        if grown >= self.checkpoint_byte_threshold {
            let mut txn_manager = recover_lock(self.txn_manager.lock(), "Database.txn_manager");
            write_checkpoint(&self.buffer_pool, &mut txn_manager)?;
            drop(txn_manager);
            self.bytes_at_last_checkpoint = self.buffer_pool.log_bytes_appended();
        }
        Ok(())
    }

    pub fn execute(&mut self, sql: &str) -> Result<ResultSet> {
        let stmt_id = self.next_stmt_id;
        self.next_stmt_id += 1;

        let txn_span = self.txn_span.clone();
        let _txn_guard = txn_span.as_ref().map(tracing::Span::enter);

        let start = std::time::Instant::now();
        let result = self.execute_impl(sql, stmt_id);
        let elapsed_ms = start.elapsed().as_millis() as u64;

        match &result {
            Ok(_) => {
                tracing::info!(fingerprint = %sql::fingerprint(sql), "statement executed");
                if elapsed_ms >= self.slow_query_warn_threshold_ms {
                    tracing::warn!(duration_ms = elapsed_ms, "slow query");
                }
            }
            Err(err) => {
                let sql_state = err.sql_state();
                let fingerprint = sql::fingerprint(sql);
                tracing::debug!(sql_state = %sql_state, %err, "statement failed");
                match err.severity() {
                    Severity::Error => {
                        tracing::warn!(
                            sql_state = %sql_state,
                            error = %err.redacted(),
                            fingerprint = %fingerprint,
                            "statement failed"
                        );
                    }
                    Severity::Fatal | Severity::Panic => {
                        tracing::error!(
                            sql_state = %sql_state,
                            error = %err.redacted(),
                            fingerprint = %fingerprint,
                            "statement failed"
                        );
                    }
                }
            }
        }
        result
    }

    #[tracing::instrument(skip_all, fields(stmt_id = stmt_id, statement_kind = tracing::field::Empty))]
    fn execute_impl(&mut self, sql: &str, stmt_id: u64) -> Result<ResultSet> {
        tracing::debug!(sql, "parsing statement");
        let tokens = Lexer::new(sql).tokenize().map_err(|err| syntax_error(&err, sql))?;
        let statement = Parser::new(tokens).parse().map_err(|err| syntax_error(&err, sql))?;
        let kind = statement_kind(&statement);
        tracing::Span::current().record("statement_kind", kind);

        let query_start = std::time::Instant::now();
        let result = match statement {
            Statement::Begin => self.handle_begin(),
            Statement::Commit => self.handle_commit(),
            Statement::Rollback => self.handle_rollback(),
            explain @ Statement::Explain { .. } => self.handle_explain(explain),
            other => self.execute_non_control_statement(other),
        };
        metrics::histogram!("query_duration_seconds", "statement_kind" => kind)
            .record(query_start.elapsed().as_secs_f64());
        result
    }

    fn execute_non_control_statement(&mut self, statement: Statement) -> Result<ResultSet> {
        let (txn_id, autocommit) = self.txn_for_statement()?;
        let bound = Binder::new(&self.catalog).bind(statement).map_err(Error::from);
        let result = bound.and_then(|bound| self.execute_bound(bound, txn_id));

        if autocommit {
            match &result {
                Ok(_) => {
                    recover_lock(self.txn_manager.lock(), "Database.txn_manager")
                        .commit(txn_id, &self.buffer_pool)?;
                    self.maybe_checkpoint()?;
                }
                Err(_) => {
                    let _ = recover_lock(self.txn_manager.lock(), "Database.txn_manager")
                        .abort(txn_id, &self.buffer_pool);
                    let _ = self.reload_catalog();
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
        let isolation_level = IsolationLevel::ReadCommitted;
        let txn_id = recover_lock(self.txn_manager.lock(), "Database.txn_manager")
            .begin(&self.buffer_pool, isolation_level)?;
        self.txn_slot = TxnSlot::Active(txn_id);
        self.txn_span = Some(
            tracing::info_span!("transaction", txn_id = txn_id.0, isolation = ?isolation_level),
        );
        Ok(ResultSet::rows_affected(0))
    }

    fn handle_commit(&mut self) -> Result<ResultSet> {
        match self.txn_slot {
            TxnSlot::None => Err(Error::NoActiveTransaction { statement: "COMMIT".to_string() }),
            TxnSlot::Aborted(txn_id) => {
                recover_lock(self.txn_manager.lock(), "Database.txn_manager")
                    .abort(txn_id, &self.buffer_pool)?;
                self.reload_catalog()?;
                self.txn_slot = TxnSlot::None;
                self.txn_span = None;
                self.maybe_checkpoint()?;
                Ok(ResultSet::RolledBack)
            }
            TxnSlot::Active(txn_id) => {
                recover_lock(self.txn_manager.lock(), "Database.txn_manager")
                    .commit(txn_id, &self.buffer_pool)?;
                self.txn_slot = TxnSlot::None;
                self.txn_span = None;
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
        recover_lock(self.txn_manager.lock(), "Database.txn_manager")
            .abort(txn_id, &self.buffer_pool)?;
        self.reload_catalog()?;

        self.txn_slot = TxnSlot::None;
        self.txn_span = None;
        self.maybe_checkpoint()?;
        Ok(ResultSet::rows_affected(0))
    }

    fn handle_explain(&mut self, statement: Statement) -> Result<ResultSet> {
        let bound = Binder::new(&self.catalog).bind(statement).map_err(Error::from)?;
        let BoundStatement::Explain { verbose, inner } = bound else {
            unreachable!(
                "handle_explain is only called for Statement::Explain, whose binder output is \
                 always BoundStatement::Explain"
            )
        };

        let logical = planner::plan(*inner)?;
        let optimized =
            Optimizer::new(vec![Box::new(IndexScanRule)]).optimize(logical, &self.catalog);
        let physical = to_physical(optimized.clone());

        let mut lines = Vec::new();
        if verbose {
            lines.push("Logical plan:".to_string());
            lines.extend(explain_logical(&optimized, &self.catalog, verbose));
            lines.push("Physical plan:".to_string());
        }
        lines.extend(explain_physical(&physical, &self.catalog, verbose));

        let rows = lines.into_iter().map(|line| Tuple::new(vec![Value::Varchar(line)])).collect();
        Ok(ResultSet::rows(vec!["QUERY PLAN".to_string()], rows))
    }

    fn reload_catalog(&mut self) -> Result<()> {
        let reload_txn = recover_lock(self.txn_manager.lock(), "Database.txn_manager")
            .begin(&self.buffer_pool, IsolationLevel::ReadCommitted)?;
        self.catalog = Catalog::open(&self.buffer_pool, reload_txn)?;
        recover_lock(self.txn_manager.lock(), "Database.txn_manager")
            .commit(reload_txn, &self.buffer_pool)?;
        Ok(())
    }

    fn txn_for_statement(&mut self) -> Result<(TxnId, bool)> {
        match self.txn_slot {
            TxnSlot::Active(txn_id) => Ok((txn_id, false)),
            TxnSlot::Aborted(_) => Err(Error::TransactionAborted),
            TxnSlot::None => {
                let txn_id = recover_lock(self.txn_manager.lock(), "Database.txn_manager")
                    .begin(&self.buffer_pool, IsolationLevel::ReadCommitted)?;
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
                tracing::debug!(plan = ?physical, "executing plan");
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
                let logical = planner::plan(BoundStatement::Select(select))?;
                let optimized =
                    Optimizer::new(vec![Box::new(IndexScanRule)]).optimize(logical, &self.catalog);
                let physical = to_physical(optimized);
                tracing::debug!(plan = ?physical, "executing plan");
                let rows = self.run(physical, txn_id)?;
                Ok(ResultSet::rows(column_names, rows))
            }
            BoundStatement::CreateIndex(create) => {
                let index_id = self
                    .catalog
                    .create_index(
                        &self.buffer_pool,
                        txn_id,
                        &create.index_name,
                        create.table_id,
                        create.column_index,
                    )?
                    .index_id;
                self.populate_index(txn_id, create.table_id, create.column_index, index_id)?;
                Ok(ResultSet::rows_affected(0))
            }
            BoundStatement::Explain { .. } => unreachable!(
                "Statement::Explain is intercepted in execute_impl and handled entirely by \
                 handle_explain, which unwraps its own BoundStatement::Explain itself rather \
                 than ever calling execute_bound with one"
            ),
        }
    }

    fn populate_index(
        &self,
        txn_id: TxnId,
        table_id: common::TableId,
        column_index: usize,
        index_id: common::IndexId,
    ) -> Result<()> {
        let table = self.catalog.get_table_by_id(table_id)?;
        let first_page_id = table.first_page_id;
        let column_types: Vec<_> =
            table.schema.columns().iter().map(|column| column.data_type).collect();

        let mut root_page_id = self.catalog.index_root_page(index_id)?;
        let heap = TableHeap::open(&self.buffer_pool, first_page_id);
        for entry in heap.iter() {
            let (rid, bytes) = entry?;
            let tuple = Tuple::decode(&bytes, &column_types)
                .map_err(|err| Error::DataCorrupted { detail: err.to_string() })?;
            let value = &tuple.values()[column_index];
            let mut key = Vec::new();
            value.encode_memcomparable(&mut key).map_err(StorageError::from)?;

            let mut btree_index = BTreeIndex::open(&self.buffer_pool, root_page_id);
            btree_index.insert(txn_id, &key, rid)?;
            let root_after = btree_index.root_page_id();
            if root_after != root_page_id {
                self.catalog.update_index_root_page(
                    &self.buffer_pool,
                    txn_id,
                    index_id,
                    root_after,
                )?;
                root_page_id = root_after;
            }
        }
        Ok(())
    }

    pub fn table_names(&self) -> Vec<String> {
        self.catalog.table_names().into_iter().map(String::from).collect()
    }

    pub fn table_schema(&self, name: &str) -> Result<Schema> {
        Ok(self.catalog.get_table(name)?.schema.clone())
    }

    fn run(&self, physical: PhysicalPlan, txn_id: TxnId) -> Result<Vec<Tuple>> {
        let txn_manager = recover_lock(self.txn_manager.lock(), "Database.txn_manager");
        let txn = txn_manager.get(txn_id)?;
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

fn new_connection_span() -> tracing::span::EnteredSpan {
    let conn_id = NEXT_CONN_ID.fetch_add(1, Ordering::Relaxed);
    let span = tracing::info_span!("connection", conn_id).entered();
    tracing::info!("connection opened");
    span
}

fn statement_kind(statement: &Statement) -> &'static str {
    match statement {
        Statement::Select(_) => "SELECT",
        Statement::Insert(_) => "INSERT",
        Statement::CreateTable(_) => "CREATE TABLE",
        Statement::CreateIndex(_) => "CREATE INDEX",
        Statement::Begin => "BEGIN",
        Statement::Commit => "COMMIT",
        Statement::Rollback => "ROLLBACK",
        Statement::Explain { .. } => "EXPLAIN",
    }
}

fn syntax_error(err: &SqlError, sql: &str) -> Error {
    Error::Syntax { message: err.render(sql), offset: err.offset(sql) }
}
