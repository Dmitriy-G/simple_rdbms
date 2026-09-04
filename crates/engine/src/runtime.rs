use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock, TryLockError, mpsc};
use std::time::{Duration, Instant};

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
use crate::worker_pool::WorkerPool;

const REPLACER_K: usize = 2;
const REQUEST_CHANNEL_CAPACITY: usize = 64;
const TICK_INTERVAL: Duration = Duration::from_millis(50);
const WORKER_POOL_SIZE: usize = 8;

static NEXT_SESSION_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct SessionId(u64);

#[derive(Debug, Clone, Copy)]
enum TxnSlot {
    None,
    Active(TxnId),
    Aborted(TxnId),
    TimedOut,
}

impl TxnSlot {
    fn txn_id(self) -> Option<TxnId> {
        match self {
            TxnSlot::None | TxnSlot::TimedOut => None,
            TxnSlot::Active(txn_id) | TxnSlot::Aborted(txn_id) => Some(txn_id),
        }
    }
}

struct SessionState {
    txn_slot: TxnSlot,
    txn_span: Option<tracing::Span>,
    conn_span: tracing::Span,
    next_stmt_id: u64,
    idle_since: Option<Instant>,
}

impl SessionState {
    fn new(conn_span: tracing::Span) -> Self {
        Self {
            txn_slot: TxnSlot::None,
            txn_span: None,
            conn_span,
            next_stmt_id: 1,
            idle_since: None,
        }
    }
}

enum EngineMessage {
    Connect {
        reply: mpsc::SyncSender<SessionId>,
    },
    Execute {
        session_id: SessionId,
        sql: String,
        reply: mpsc::SyncSender<Result<ResultSet>>,
    },
    TableNames {
        reply: mpsc::SyncSender<Vec<String>>,
    },
    TableSchema {
        name: String,
        reply: mpsc::SyncSender<Result<Schema>>,
    },
    Checkpoint {
        reply: mpsc::SyncSender<Result<()>>,
    },
    BestEffortFlush {
        reply: mpsc::SyncSender<()>,
    },
    Disconnect {
        session_id: SessionId,
    },
    #[cfg(feature = "test-util")]
    Shutdown,
    #[cfg(feature = "test-util")]
    Stats {
        reply: mpsc::SyncSender<EngineStats>,
    },
}

fn engine_unavailable() -> Error {
    Error::EngineUnavailable { detail: "the engine thread's reply channel is closed".to_string() }
}

fn unknown_session(session_id: SessionId) -> Error {
    Error::UnknownSession {
        detail: format!("session {} may have already disconnected", session_id.0),
    }
}

pub(crate) struct EngineHandle {
    sender: Option<mpsc::SyncSender<EngineMessage>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl EngineHandle {
    fn send(&self, message: EngineMessage) -> Result<()> {
        self.sender
            .as_ref()
            .ok_or_else(engine_unavailable)?
            .send(message)
            .map_err(|_| engine_unavailable())
    }

    pub(crate) fn open(config: &DbConfig) -> Result<Arc<Self>> {
        let disk_manager = DiskManager::open(config.db_path.clone(), config.page_size)?;
        let mut wal_path = config.db_path.clone().into_os_string();
        wal_path.push(".wal");
        let log_manager = LogManager::open(wal_path)?;
        let mut dwb_path = config.db_path.clone().into_os_string();
        dwb_path.push(".dwb");
        let dwb = DoubleWriteBuffer::open(dwb_path, config.dwb_capacity)?;
        Self::spawn(config, disk_manager, dwb, log_manager)
    }

    #[cfg(feature = "test-util")]
    pub(crate) fn open_with_devices(
        config: &DbConfig,
        db_device: Box<dyn storage::block_device::BlockDevice>,
        wal_store: Arc<dyn storage::wal::SegmentStore>,
        wal_segment_size: u64,
        dwb_device: Box<dyn storage::block_device::BlockDevice>,
    ) -> Result<Arc<Self>> {
        let disk_manager = DiskManager::open_with_device(db_device, config.page_size, None)?;
        let log_manager = LogManager::open_with_segment_store(wal_store, wal_segment_size)?;
        let dwb = DoubleWriteBuffer::open_with_device(dwb_device, config.dwb_capacity)?;
        Self::spawn(config, disk_manager, dwb, log_manager)
    }

    fn spawn(
        config: &DbConfig,
        disk_manager: DiskManager,
        dwb: DoubleWriteBuffer,
        log_manager: LogManager,
    ) -> Result<Arc<Self>> {
        let shared = Arc::new(EngineShared::open(config, disk_manager, dwb, log_manager)?);
        let (sender, receiver) = mpsc::sync_channel(REQUEST_CHANNEL_CAPACITY);
        let dispatch = tracing::dispatcher::get_default(|dispatch| dispatch.clone());
        let worker_pool = WorkerPool::new(WORKER_POOL_SIZE, dispatch.clone()).map_err(Error::Io)?;
        let thread = std::thread::Builder::new()
            .name("engine".to_string())
            .spawn(move || {
                tracing::dispatcher::with_default(&dispatch, || {
                    run_engine(EngineState { shared, worker_pool }, receiver);
                });
            })
            .map_err(Error::Io)?;
        Ok(Arc::new(Self { sender: Some(sender), thread: Some(thread) }))
    }

    pub(crate) fn connect(self: &Arc<Self>) -> Result<SessionHandle> {
        let (reply, reply_rx) = mpsc::sync_channel(1);
        self.send(EngineMessage::Connect { reply })?;
        let session_id = reply_rx.recv().map_err(|_| engine_unavailable())?;
        Ok(SessionHandle { engine: Arc::clone(self), session_id })
    }
}

impl Drop for EngineHandle {
    fn drop(&mut self) {
        self.sender.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

pub(crate) struct SessionHandle {
    engine: Arc<EngineHandle>,
    session_id: SessionId,
}

impl SessionHandle {
    pub(crate) fn connect(&self) -> Result<SessionHandle> {
        self.engine.connect()
    }

    pub(crate) fn execute(&self, sql: &str) -> Result<ResultSet> {
        let (reply, reply_rx) = mpsc::sync_channel(1);
        self.engine.send(EngineMessage::Execute {
            session_id: self.session_id,
            sql: sql.to_string(),
            reply,
        })?;
        reply_rx.recv().map_err(|_| engine_unavailable())?
    }

    pub(crate) fn table_names(&self) -> Result<Vec<String>> {
        let (reply, reply_rx) = mpsc::sync_channel(1);
        self.engine.send(EngineMessage::TableNames { reply })?;
        reply_rx.recv().map_err(|_| engine_unavailable())
    }

    pub(crate) fn table_schema(&self, name: &str) -> Result<Schema> {
        let (reply, reply_rx) = mpsc::sync_channel(1);
        self.engine.send(EngineMessage::TableSchema { name: name.to_string(), reply })?;
        reply_rx.recv().map_err(|_| engine_unavailable())?
    }

    pub(crate) fn checkpoint_and_flush(&self) -> Result<()> {
        let (reply, reply_rx) = mpsc::sync_channel(1);
        self.engine.send(EngineMessage::Checkpoint { reply })?;
        reply_rx.recv().map_err(|_| engine_unavailable())?
    }

    pub(crate) fn best_effort_flush(&self) {
        let (reply, reply_rx) = mpsc::sync_channel(1);
        if self.engine.send(EngineMessage::BestEffortFlush { reply }).is_ok() {
            let _ = reply_rx.recv();
        }
    }

    #[cfg(feature = "test-util")]
    pub(crate) fn kill_engine_for_test(&self) {
        let _ = self.engine.send(EngineMessage::Shutdown);
    }

    #[cfg(feature = "test-util")]
    pub(crate) fn stats(&self) -> Result<EngineStats> {
        let (reply, reply_rx) = mpsc::sync_channel(1);
        self.engine.send(EngineMessage::Stats { reply })?;
        reply_rx.recv().map_err(|_| engine_unavailable())
    }
}

impl Drop for SessionHandle {
    fn drop(&mut self) {
        let _ = self.engine.send(EngineMessage::Disconnect { session_id: self.session_id });
    }
}

struct EngineState {
    shared: Arc<EngineShared>,
    worker_pool: WorkerPool,
}

fn run_engine(state: EngineState, receiver: mpsc::Receiver<EngineMessage>) {
    let mut sessions: HashMap<SessionId, Arc<Mutex<SessionState>>> = HashMap::new();

    loop {
        match receiver.recv_timeout(TICK_INTERVAL) {
            Ok(message) => {
                if !dispatch_message(&state, &mut sessions, message) {
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => tick(&state, &sessions),
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
}

fn dispatch_message(
    state: &EngineState,
    sessions: &mut HashMap<SessionId, Arc<Mutex<SessionState>>>,
    message: EngineMessage,
) -> bool {
    match message {
        EngineMessage::Connect { reply } => {
            let session_id = SessionId(NEXT_SESSION_ID.fetch_add(1, Ordering::Relaxed));
            let conn_span = new_connection_span(session_id);
            sessions.insert(session_id, Arc::new(Mutex::new(SessionState::new(conn_span))));
            let _ = reply.send(session_id);
        }
        EngineMessage::Execute { session_id, sql, reply } => {
            dispatch_execute(state, sessions, session_id, sql, reply);
        }
        EngineMessage::TableNames { reply } => {
            let _ = reply.send(state.shared.table_names());
        }
        EngineMessage::TableSchema { name, reply } => {
            let _ = reply.send(state.shared.table_schema(&name));
        }
        EngineMessage::Checkpoint { reply } => {
            let _ = reply.send(state.shared.checkpoint_and_flush());
        }
        EngineMessage::BestEffortFlush { reply } => {
            state.shared.best_effort_flush();
            let _ = reply.send(());
        }
        EngineMessage::Disconnect { session_id } => {
            disconnect(state, sessions, session_id);
        }
        #[cfg(feature = "test-util")]
        EngineMessage::Shutdown => return false,
        #[cfg(feature = "test-util")]
        EngineMessage::Stats { reply } => {
            let _ = reply.send(state.shared.stats());
        }
    }
    true
}

fn tick(state: &EngineState, sessions: &HashMap<SessionId, Arc<Mutex<SessionState>>>) {
    expire_idle_transaction(state, sessions);
}

fn dispatch_execute(
    state: &EngineState,
    sessions: &HashMap<SessionId, Arc<Mutex<SessionState>>>,
    session_id: SessionId,
    sql: String,
    reply: mpsc::SyncSender<Result<ResultSet>>,
) {
    let Some(session) = sessions.get(&session_id) else {
        let _ = reply.send(Err(unknown_session(session_id)));
        return;
    };
    let session = Arc::clone(session);
    let shared = Arc::clone(&state.shared);
    state.worker_pool.submit(Box::new(move || {
        let mut session = recover_lock(session.lock(), "SessionState");
        let result = shared.execute(&mut session, &sql);
        let _ = reply.send(result);
    }));
}

fn expire_idle_transaction(
    state: &EngineState,
    sessions: &HashMap<SessionId, Arc<Mutex<SessionState>>>,
) {
    for (&session_id, session) in sessions {
        let overdue = {
            let session = match session.try_lock() {
                Ok(guard) => guard,
                Err(TryLockError::WouldBlock) => continue,
                Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            };
            session.txn_slot.txn_id().is_some()
                && session.idle_since.is_some_and(|idle_since| {
                    idle_since.elapsed() >= state.shared.idle_in_transaction_timeout
                })
        };
        if !overdue {
            continue;
        }

        let session = Arc::clone(session);
        let shared = Arc::clone(&state.shared);
        state.worker_pool.submit(Box::new(move || {
            expire_idle_session(&shared, &session, session_id);
        }));
    }
}

fn expire_idle_session(
    shared: &EngineShared,
    session: &Mutex<SessionState>,
    session_id: SessionId,
) {
    let mut session = recover_lock(session.lock(), "SessionState");
    let Some(txn_id) = session.txn_slot.txn_id() else { return };
    let Some(idle_since) = session.idle_since else { return };
    if idle_since.elapsed() < shared.idle_in_transaction_timeout {
        return;
    }

    if let Err(err) = shared.abort_txn(txn_id) {
        tracing::warn!(
            sql_state = %err.sql_state(),
            error = %err.redacted(),
            "idle-in-transaction abort failed"
        );
    }
    if let Err(err) = shared.reload_catalog() {
        tracing::warn!(
            sql_state = %err.sql_state(),
            error = %err.redacted(),
            "catalog reload after idle-in-transaction abort failed"
        );
    }

    session.txn_slot = TxnSlot::TimedOut;
    session.txn_span = None;
    session.idle_since = None;
    tracing::warn!(
        session_id = session_id.0,
        txn_id = txn_id.0,
        "idle-in-transaction timeout; transaction aborted"
    );
}

fn disconnect(
    state: &EngineState,
    sessions: &mut HashMap<SessionId, Arc<Mutex<SessionState>>>,
    session_id: SessionId,
) {
    let Some(session) = sessions.remove(&session_id) else { return };
    let shared = Arc::clone(&state.shared);
    state.worker_pool.submit(Box::new(move || {
        disconnect_session(&shared, &session, session_id);
    }));
}

fn disconnect_session(shared: &EngineShared, session: &Mutex<SessionState>, session_id: SessionId) {
    let session = recover_lock(session.lock(), "SessionState");
    let Some(txn_id) = session.txn_slot.txn_id() else { return };

    if let Err(err) = shared.abort_txn(txn_id) {
        tracing::warn!(
            sql_state = %err.sql_state(),
            error = %err.redacted(),
            "abort on disconnect failed"
        );
    }
    if let Err(err) = shared.reload_catalog() {
        tracing::warn!(
            sql_state = %err.sql_state(),
            error = %err.redacted(),
            "catalog reload on disconnect failed"
        );
    }
    tracing::warn!(
        session_id = session_id.0,
        txn_id = txn_id.0,
        "session disconnected with an open transaction; rolled back"
    );
}

struct CheckpointState {
    bytes_at_last_checkpoint: u64,
    checkpoints_written: u64,
}

struct EngineShared {
    catalog: RwLock<Catalog>,
    buffer_pool: Arc<BufferPool>,
    txn_manager: Mutex<TransactionManager>,
    checkpoint: Mutex<CheckpointState>,
    checkpoint_byte_threshold: u64,
    slow_query_warn_threshold_ms: u64,
    idle_in_transaction_timeout: Duration,
}

#[cfg(feature = "test-util")]
#[derive(Debug, Clone, Copy, Default)]
pub struct EngineStats {
    pub checkpoints_written: u64,
}

impl EngineShared {
    fn open(
        config: &DbConfig,
        disk_manager: DiskManager,
        dwb: DoubleWriteBuffer,
        log_manager: LogManager,
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
            catalog: RwLock::new(catalog),
            buffer_pool,
            txn_manager: Mutex::new(txn_manager),
            checkpoint: Mutex::new(CheckpointState {
                bytes_at_last_checkpoint,
                checkpoints_written: 0,
            }),
            checkpoint_byte_threshold: config.checkpoint_byte_threshold,
            slow_query_warn_threshold_ms: config.slow_query_warn_threshold_ms,
            idle_in_transaction_timeout: Duration::from_millis(
                config.idle_in_transaction_timeout_ms,
            ),
        })
    }

    fn record_checkpoint_written(&self) {
        let mut checkpoint = recover_lock(self.checkpoint.lock(), "EngineShared.checkpoint");
        checkpoint.checkpoints_written += 1;
        metrics::counter!("checkpoints_written_total").increment(1);
    }

    #[cfg(feature = "test-util")]
    fn stats(&self) -> EngineStats {
        let checkpoint = recover_lock(self.checkpoint.lock(), "EngineShared.checkpoint");
        EngineStats { checkpoints_written: checkpoint.checkpoints_written }
    }

    fn checkpoint_and_flush(&self) -> Result<()> {
        {
            let mut txn_manager = recover_lock(self.txn_manager.lock(), "EngineShared.txn_manager");
            write_checkpoint(&self.buffer_pool, &mut txn_manager)?;
        }
        self.record_checkpoint_written();
        self.buffer_pool.flush_log_all()?;
        self.buffer_pool.flush_all()?;
        self.buffer_pool.sync()?;
        Ok(())
    }

    fn best_effort_flush(&self) {
        if self.buffer_pool.is_flush_poisoned() {
            return;
        }
        let _ = self.buffer_pool.flush_log_all();
        let _ = self.buffer_pool.flush_all();
    }

    fn maybe_checkpoint(&self) {
        if self.buffer_pool.is_flush_poisoned() {
            return;
        }
        let bytes_at_last_checkpoint =
            recover_lock(self.checkpoint.lock(), "EngineShared.checkpoint")
                .bytes_at_last_checkpoint;
        let grown = self.buffer_pool.log_bytes_appended() - bytes_at_last_checkpoint;
        if grown < self.checkpoint_byte_threshold {
            return;
        }
        let result = {
            let mut txn_manager = recover_lock(self.txn_manager.lock(), "EngineShared.txn_manager");
            write_checkpoint(&self.buffer_pool, &mut txn_manager)
        };
        match result {
            Ok(_) => {
                let mut checkpoint =
                    recover_lock(self.checkpoint.lock(), "EngineShared.checkpoint");
                checkpoint.bytes_at_last_checkpoint = self.buffer_pool.log_bytes_appended();
                drop(checkpoint);
                self.record_checkpoint_written();
            }
            Err(err) => {
                let err = Error::from(err);
                tracing::warn!(
                    sql_state = %err.sql_state(),
                    error = %err.redacted(),
                    "checkpoint attempt failed; the triggering statement's own effects are \
                     unaffected and a later statement will retry"
                );
            }
        }
    }

    fn table_names(&self) -> Vec<String> {
        let catalog = recover_lock(self.catalog.read(), "EngineShared.catalog");
        catalog.table_names().into_iter().map(String::from).collect()
    }

    fn table_schema(&self, name: &str) -> Result<Schema> {
        let catalog = recover_lock(self.catalog.read(), "EngineShared.catalog");
        Ok(catalog.get_table(name)?.schema.clone())
    }

    fn execute(&self, session: &mut SessionState, sql: &str) -> Result<ResultSet> {
        let stmt_id = session.next_stmt_id;
        session.next_stmt_id += 1;

        let conn_span = session.conn_span.clone();
        let _conn_guard = conn_span.enter();
        let txn_span = session.txn_span.clone();
        let _txn_guard = txn_span.as_ref().map(tracing::Span::enter);

        let start = Instant::now();
        let result = if self.buffer_pool.is_flush_poisoned() {
            Err(Error::FlushPoisoned)
        } else {
            self.execute_impl(session, sql, stmt_id)
        };
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

        session.idle_since =
            if session.txn_slot.txn_id().is_some() { Some(Instant::now()) } else { None };

        result
    }

    #[tracing::instrument(
        skip_all,
        fields(stmt_id = stmt_id, statement_kind = tracing::field::Empty)
    )]
    fn execute_impl(
        &self,
        session: &mut SessionState,
        sql: &str,
        stmt_id: u64,
    ) -> Result<ResultSet> {
        if matches!(session.txn_slot, TxnSlot::TimedOut) {
            session.txn_slot = TxnSlot::None;
            session.txn_span = None;
            return Err(Error::IdleInTransactionTimeout);
        }

        tracing::debug!(sql, "parsing statement");
        let tokens = Lexer::new(sql).tokenize().map_err(|err| syntax_error(&err, sql))?;
        let statement = Parser::new(tokens).parse().map_err(|err| syntax_error(&err, sql))?;
        let kind = statement_kind(&statement);
        tracing::Span::current().record("statement_kind", kind);

        let query_start = Instant::now();
        let result = match statement {
            Statement::Begin => self.handle_begin(session),
            Statement::Commit => self.handle_commit(session),
            Statement::Rollback => self.handle_rollback(session),
            explain @ Statement::Explain { .. } => self.handle_explain(explain),
            other => self.execute_non_control_statement(session, other),
        };
        metrics::histogram!("query_duration_seconds", "statement_kind" => kind)
            .record(query_start.elapsed().as_secs_f64());
        result
    }

    fn execute_non_control_statement(
        &self,
        session: &mut SessionState,
        statement: Statement,
    ) -> Result<ResultSet> {
        let (txn_id, autocommit) = self.txn_for_statement(session)?;
        let bound = {
            let catalog = recover_lock(self.catalog.read(), "EngineShared.catalog");
            Binder::new(&catalog).bind(statement).map_err(Error::from)
        };
        let result = bound.and_then(|bound| self.execute_bound(bound, txn_id));

        if autocommit {
            match &result {
                Ok(_) => {
                    self.commit_txn(txn_id)?;
                    self.maybe_checkpoint();
                }
                Err(_) => {
                    let _ = self.abort_txn(txn_id);
                    let _ = self.reload_catalog();
                }
            }
        } else if result.is_err() {
            session.txn_slot = TxnSlot::Aborted(txn_id);
        }
        result
    }

    fn handle_begin(&self, session: &mut SessionState) -> Result<ResultSet> {
        if session.txn_slot.txn_id().is_some() {
            return Err(Error::NestedTransaction);
        }
        let isolation_level = IsolationLevel::ReadCommitted;
        let txn_id = {
            let mut txn_manager = recover_lock(self.txn_manager.lock(), "EngineShared.txn_manager");
            txn_manager.begin(&self.buffer_pool, isolation_level)?
        };
        session.txn_slot = TxnSlot::Active(txn_id);
        session.txn_span = Some(
            tracing::info_span!("transaction", txn_id = txn_id.0, isolation = ?isolation_level),
        );
        Ok(ResultSet::rows_affected(0))
    }

    fn handle_commit(&self, session: &mut SessionState) -> Result<ResultSet> {
        match session.txn_slot {
            TxnSlot::None | TxnSlot::TimedOut => {
                Err(Error::NoActiveTransaction { statement: "COMMIT".to_string() })
            }
            TxnSlot::Aborted(txn_id) => {
                self.abort_txn(txn_id)?;
                self.reload_catalog()?;
                session.txn_slot = TxnSlot::None;
                session.txn_span = None;
                self.maybe_checkpoint();
                Ok(ResultSet::RolledBack)
            }
            TxnSlot::Active(txn_id) => {
                self.commit_txn(txn_id)?;
                session.txn_slot = TxnSlot::None;
                session.txn_span = None;
                self.maybe_checkpoint();
                Ok(ResultSet::rows_affected(0))
            }
        }
    }

    fn handle_rollback(&self, session: &mut SessionState) -> Result<ResultSet> {
        let txn_id = session
            .txn_slot
            .txn_id()
            .ok_or_else(|| Error::NoActiveTransaction { statement: "ROLLBACK".to_string() })?;
        self.abort_txn(txn_id)?;
        self.reload_catalog()?;

        session.txn_slot = TxnSlot::None;
        session.txn_span = None;
        self.maybe_checkpoint();
        Ok(ResultSet::rows_affected(0))
    }

    fn handle_explain(&self, statement: Statement) -> Result<ResultSet> {
        let catalog = recover_lock(self.catalog.read(), "EngineShared.catalog");
        let bound = Binder::new(&catalog).bind(statement).map_err(Error::from)?;
        let BoundStatement::Explain { verbose, inner } = bound else {
            unreachable!(
                "handle_explain is only called for Statement::Explain, whose binder output is \
                 always BoundStatement::Explain"
            )
        };

        let logical = planner::plan(*inner)?;
        let optimized = Optimizer::new(vec![Box::new(IndexScanRule)]).optimize(logical, &catalog);
        let physical = to_physical(optimized.clone());

        let mut lines = Vec::new();
        if verbose {
            lines.push("Logical plan:".to_string());
            lines.extend(explain_logical(&optimized, &catalog, verbose));
            lines.push("Physical plan:".to_string());
        }
        lines.extend(explain_physical(&physical, &catalog, verbose));

        let rows = lines.into_iter().map(|line| Tuple::new(vec![Value::Varchar(line)])).collect();
        Ok(ResultSet::rows(vec!["QUERY PLAN".to_string()], rows))
    }

    fn reload_catalog(&self) -> Result<()> {
        let reload_txn = {
            let mut txn_manager = recover_lock(self.txn_manager.lock(), "EngineShared.txn_manager");
            txn_manager.begin(&self.buffer_pool, IsolationLevel::ReadCommitted)?
        };
        let fresh = Catalog::open(&self.buffer_pool, reload_txn)?;
        {
            let mut txn_manager = recover_lock(self.txn_manager.lock(), "EngineShared.txn_manager");
            txn_manager.commit(reload_txn, &self.buffer_pool)?;
        }
        *recover_lock(self.catalog.write(), "EngineShared.catalog") = fresh;
        Ok(())
    }

    fn commit_txn(&self, txn_id: TxnId) -> Result<()> {
        let mut txn_manager = recover_lock(self.txn_manager.lock(), "EngineShared.txn_manager");
        txn_manager.commit(txn_id, &self.buffer_pool)?;
        Ok(())
    }

    fn abort_txn(&self, txn_id: TxnId) -> Result<()> {
        let mut txn_manager = recover_lock(self.txn_manager.lock(), "EngineShared.txn_manager");
        txn_manager.abort(txn_id, &self.buffer_pool)?;
        Ok(())
    }

    fn txn_for_statement(&self, session: &mut SessionState) -> Result<(TxnId, bool)> {
        match session.txn_slot {
            TxnSlot::Active(txn_id) => Ok((txn_id, false)),
            TxnSlot::Aborted(_) => Err(Error::TransactionAborted),
            TxnSlot::None | TxnSlot::TimedOut => {
                let mut txn_manager =
                    recover_lock(self.txn_manager.lock(), "EngineShared.txn_manager");
                let txn_id = txn_manager.begin(&self.buffer_pool, IsolationLevel::ReadCommitted)?;
                Ok((txn_id, true))
            }
        }
    }

    fn execute_bound(&self, bound: BoundStatement, txn_id: TxnId) -> Result<ResultSet> {
        match bound {
            BoundStatement::CreateTable(create) => {
                let schema = Schema::new(
                    create
                        .columns
                        .into_iter()
                        .map(|column| Column::new(column.name, column.data_type, column.nullable))
                        .collect(),
                );
                let mut catalog = recover_lock(self.catalog.write(), "EngineShared.catalog");
                catalog.create_table(&self.buffer_pool, txn_id, &create.table_name, schema)?;
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
                let optimized = {
                    let catalog = recover_lock(self.catalog.read(), "EngineShared.catalog");
                    Optimizer::new(vec![Box::new(IndexScanRule)]).optimize(logical, &catalog)
                };
                let physical = to_physical(optimized);
                tracing::debug!(plan = ?physical, "executing plan");
                let rows = self.run(physical, txn_id)?;
                Ok(ResultSet::rows(column_names, rows))
            }
            BoundStatement::CreateIndex(create) => {
                let mut catalog = recover_lock(self.catalog.write(), "EngineShared.catalog");
                let index_id = catalog
                    .create_index(
                        &self.buffer_pool,
                        txn_id,
                        &create.index_name,
                        create.table_id,
                        create.column_index,
                    )?
                    .index_id;
                self.populate_index(
                    &catalog,
                    txn_id,
                    create.table_id,
                    create.column_index,
                    index_id,
                )?;
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
        catalog: &Catalog,
        txn_id: TxnId,
        table_id: common::TableId,
        column_index: usize,
        index_id: common::IndexId,
    ) -> Result<()> {
        let table = catalog.get_table_by_id(table_id)?;
        let first_page_id = table.first_page_id;
        let column_types: Vec<_> =
            table.schema.columns().iter().map(|column| column.data_type).collect();

        let mut root_page_id = catalog.index_root_page(index_id)?;
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
                catalog.update_index_root_page(&self.buffer_pool, txn_id, index_id, root_after)?;
                root_page_id = root_after;
            }
        }
        Ok(())
    }

    fn run(&self, physical: PhysicalPlan, txn_id: TxnId) -> Result<Vec<Tuple>> {
        let txn = {
            let txn_manager = recover_lock(self.txn_manager.lock(), "EngineShared.txn_manager");
            txn_manager.get(txn_id)?.clone()
        };
        let catalog = recover_lock(self.catalog.read(), "EngineShared.catalog");
        let mut executor = build_executor(physical);
        let mut ctx = ExecutorContext::new(&catalog, &self.buffer_pool, &txn);
        executor.init(&mut ctx)?;

        let mut rows = Vec::new();
        while let Some(tuple) = executor.next(&mut ctx)? {
            rows.push(tuple);
        }
        Ok(rows)
    }
}

fn new_connection_span(session_id: SessionId) -> tracing::Span {
    let span = tracing::info_span!("connection", conn_id = session_id.0);
    let _enter = span.enter();
    tracing::info!("connection opened");
    drop(_enter);
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
