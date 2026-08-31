use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use catalog::{Catalog, Column, Schema};
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
const REQUEST_CHANNEL_CAPACITY: usize = 64;
const TICK_INTERVAL: Duration = Duration::from_millis(50);

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
    session_id: SessionId,
    txn_slot: TxnSlot,
    txn_span: Option<tracing::Span>,
    conn_span: tracing::Span,
    next_stmt_id: u64,
    idle_since: Option<Instant>,
}

impl SessionState {
    fn new(session_id: SessionId, conn_span: tracing::Span) -> Self {
        Self {
            session_id,
            txn_slot: TxnSlot::None,
            txn_span: None,
            conn_span,
            next_stmt_id: 1,
            idle_since: None,
        }
    }
}

struct ParkedRequest {
    session_id: SessionId,
    sql: String,
    reply: mpsc::SyncSender<Result<ResultSet>>,
    deadline: Instant,
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
    #[cfg(any(test, feature = "test-util"))]
    Shutdown,
}

fn engine_unavailable() -> Error {
    Error::EngineUnavailable { detail: "the engine thread's reply channel is closed".to_string() }
}

fn unknown_session(session_id: SessionId) -> Error {
    Error::UnknownSession {
        detail: format!("session {} may have already disconnected", session_id.0),
    }
}

fn lock_timeout() -> Error {
    Error::LockTimeout {
        detail: "the session already holding an explicit transaction did not commit or roll \
                  back before the deadline"
            .to_string(),
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
        let state = EngineState::open(config, disk_manager, dwb, log_manager)?;
        let (sender, receiver) = mpsc::sync_channel(REQUEST_CHANNEL_CAPACITY);
        let dispatch = tracing::dispatcher::get_default(|dispatch| dispatch.clone());
        let thread = std::thread::Builder::new()
            .name("engine".to_string())
            .spawn(move || {
                tracing::dispatcher::with_default(&dispatch, || run_engine(state, receiver));
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

    #[cfg(any(test, feature = "test-util"))]
    pub(crate) fn kill_engine_for_test(&self) {
        let _ = self.engine.send(EngineMessage::Shutdown);
    }
}

impl Drop for SessionHandle {
    fn drop(&mut self) {
        let _ = self.engine.send(EngineMessage::Disconnect { session_id: self.session_id });
    }
}

fn run_engine(mut state: EngineState, receiver: mpsc::Receiver<EngineMessage>) {
    let mut sessions: HashMap<SessionId, SessionState> = HashMap::new();
    let mut waiters: VecDeque<ParkedRequest> = VecDeque::new();

    loop {
        match receiver.recv_timeout(TICK_INTERVAL) {
            Ok(message) => {
                if !dispatch_message(&mut state, &mut sessions, &mut waiters, message) {
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => tick(&mut state, &mut sessions, &mut waiters),
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
}

fn dispatch_message(
    state: &mut EngineState,
    sessions: &mut HashMap<SessionId, SessionState>,
    waiters: &mut VecDeque<ParkedRequest>,
    message: EngineMessage,
) -> bool {
    match message {
        EngineMessage::Connect { reply } => {
            let session_id = SessionId(NEXT_SESSION_ID.fetch_add(1, Ordering::Relaxed));
            let conn_span = new_connection_span(session_id);
            sessions.insert(session_id, SessionState::new(session_id, conn_span));
            let _ = reply.send(session_id);
        }
        EngineMessage::Execute { session_id, sql, reply } => {
            admit_execute(state, sessions, waiters, session_id, sql, reply);
        }
        EngineMessage::TableNames { reply } => {
            let _ = reply.send(state.table_names());
        }
        EngineMessage::TableSchema { name, reply } => {
            let _ = reply.send(state.table_schema(&name));
        }
        EngineMessage::Checkpoint { reply } => {
            let _ = reply.send(state.checkpoint_and_flush());
        }
        EngineMessage::BestEffortFlush { reply } => {
            state.best_effort_flush();
            let _ = reply.send(());
        }
        EngineMessage::Disconnect { session_id } => {
            disconnect(state, sessions, waiters, session_id);
        }
        #[cfg(any(test, feature = "test-util"))]
        EngineMessage::Shutdown => return false,
    }
    true
}

fn tick(
    state: &mut EngineState,
    sessions: &mut HashMap<SessionId, SessionState>,
    waiters: &mut VecDeque<ParkedRequest>,
) {
    expire_waiters(waiters);
    expire_idle_transaction(state, sessions);
    try_admit_waiters(state, sessions, waiters);
}

fn peek_needs_txn_slot(sql: &str, session: &SessionState) -> bool {
    if session.txn_slot.txn_id().is_some() {
        return false;
    }
    let Ok(tokens) = Lexer::new(sql).tokenize() else { return false };
    let Ok(statement) = Parser::new(tokens).parse() else { return false };
    !matches!(statement, Statement::Commit | Statement::Rollback | Statement::Explain { .. })
}

fn must_park(
    current_txn: Option<(SessionId, TxnId)>,
    session_id: SessionId,
    session: &SessionState,
    sql: &str,
) -> bool {
    match current_txn {
        Some((holder, _)) if holder != session_id => peek_needs_txn_slot(sql, session),
        _ => false,
    }
}

fn admit_execute(
    state: &mut EngineState,
    sessions: &mut HashMap<SessionId, SessionState>,
    waiters: &mut VecDeque<ParkedRequest>,
    session_id: SessionId,
    sql: String,
    reply: mpsc::SyncSender<Result<ResultSet>>,
) {
    let Some(session) = sessions.get_mut(&session_id) else {
        let _ = reply.send(Err(unknown_session(session_id)));
        return;
    };

    if must_park(state.current_txn, session_id, session, &sql) {
        waiters.push_back(ParkedRequest {
            session_id,
            sql,
            reply,
            deadline: Instant::now() + state.lock_wait_timeout,
        });
        return;
    }

    let result = state.execute(session, &sql);
    let _ = reply.send(result);
    try_admit_waiters(state, sessions, waiters);
}

fn try_admit_waiters(
    state: &mut EngineState,
    sessions: &mut HashMap<SessionId, SessionState>,
    waiters: &mut VecDeque<ParkedRequest>,
) {
    while state.current_txn.is_none() {
        let Some(front_deadline) = waiters.front().map(|waiter| waiter.deadline) else { break };
        if front_deadline <= Instant::now() {
            if let Some(expired) = waiters.pop_front() {
                let _ = expired.reply.send(Err(lock_timeout()));
            }
            continue;
        }

        let Some(granted) = waiters.pop_front() else { break };
        let Some(session) = sessions.get_mut(&granted.session_id) else { continue };
        let result = state.execute(session, &granted.sql);
        let _ = granted.reply.send(result);
    }
}

fn expire_waiters(waiters: &mut VecDeque<ParkedRequest>) {
    let now = Instant::now();
    while matches!(waiters.front(), Some(waiter) if waiter.deadline <= now) {
        if let Some(expired) = waiters.pop_front() {
            let _ = expired.reply.send(Err(lock_timeout()));
        }
    }
}

fn expire_idle_transaction(
    state: &mut EngineState,
    sessions: &mut HashMap<SessionId, SessionState>,
) {
    let Some((session_id, txn_id)) = state.current_txn else { return };
    let Some(session) = sessions.get_mut(&session_id) else { return };
    let Some(idle_since) = session.idle_since else { return };
    if idle_since.elapsed() < state.idle_in_transaction_timeout {
        return;
    }

    if let Err(err) = state.txn_manager.abort(txn_id, &state.buffer_pool) {
        let err = Error::from(err);
        tracing::warn!(
            sql_state = %err.sql_state(),
            error = %err.redacted(),
            "idle-in-transaction abort failed"
        );
    }
    if let Err(err) = state.reload_catalog() {
        tracing::warn!(
            sql_state = %err.sql_state(),
            error = %err.redacted(),
            "catalog reload after idle-in-transaction abort failed"
        );
    }

    state.current_txn = None;
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
    state: &mut EngineState,
    sessions: &mut HashMap<SessionId, SessionState>,
    waiters: &mut VecDeque<ParkedRequest>,
    session_id: SessionId,
) {
    waiters.retain(|waiter| waiter.session_id != session_id);

    if let Some(session) = sessions.get_mut(&session_id) {
        if let Some(txn_id) = session.txn_slot.txn_id() {
            if let Err(err) = state.txn_manager.abort(txn_id, &state.buffer_pool) {
                let err = Error::from(err);
                tracing::warn!(
                    sql_state = %err.sql_state(),
                    error = %err.redacted(),
                    "abort on disconnect failed"
                );
            }
            if let Err(err) = state.reload_catalog() {
                tracing::warn!(
                    sql_state = %err.sql_state(),
                    error = %err.redacted(),
                    "catalog reload on disconnect failed"
                );
            }
            state.current_txn = None;
            tracing::warn!(
                session_id = session_id.0,
                txn_id = txn_id.0,
                "session disconnected with an open transaction; rolled back"
            );
        }
    }

    sessions.remove(&session_id);
    try_admit_waiters(state, sessions, waiters);
}

struct EngineState {
    catalog: Catalog,
    buffer_pool: Arc<BufferPool>,
    txn_manager: TransactionManager,
    checkpoint_byte_threshold: u64,
    bytes_at_last_checkpoint: u64,
    slow_query_warn_threshold_ms: u64,
    current_txn: Option<(SessionId, TxnId)>,
    lock_wait_timeout: Duration,
    idle_in_transaction_timeout: Duration,
}

impl EngineState {
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
            catalog,
            buffer_pool,
            txn_manager,
            checkpoint_byte_threshold: config.checkpoint_byte_threshold,
            bytes_at_last_checkpoint,
            slow_query_warn_threshold_ms: config.slow_query_warn_threshold_ms,
            current_txn: None,
            lock_wait_timeout: Duration::from_millis(config.lock_wait_timeout_ms),
            idle_in_transaction_timeout: Duration::from_millis(
                config.idle_in_transaction_timeout_ms,
            ),
        })
    }

    fn checkpoint_and_flush(&mut self) -> Result<()> {
        write_checkpoint(&self.buffer_pool, &mut self.txn_manager)?;
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

    fn maybe_checkpoint(&mut self) {
        if self.buffer_pool.is_flush_poisoned() {
            return;
        }
        let grown = self.buffer_pool.log_bytes_appended() - self.bytes_at_last_checkpoint;
        if grown < self.checkpoint_byte_threshold {
            return;
        }
        let result = write_checkpoint(&self.buffer_pool, &mut self.txn_manager);
        match result {
            Ok(_) => {
                self.bytes_at_last_checkpoint = self.buffer_pool.log_bytes_appended();
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
        self.catalog.table_names().into_iter().map(String::from).collect()
    }

    fn table_schema(&self, name: &str) -> Result<Schema> {
        Ok(self.catalog.get_table(name)?.schema.clone())
    }

    fn execute(&mut self, session: &mut SessionState, sql: &str) -> Result<ResultSet> {
        let stmt_id = session.next_stmt_id;
        session.next_stmt_id += 1;

        let conn_span = session.conn_span.clone();
        let _conn_guard = conn_span.enter();
        let txn_span = session.txn_span.clone();
        let _txn_guard = txn_span.as_ref().map(tracing::Span::enter);

        let start = std::time::Instant::now();
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

        session.idle_since = if session.txn_slot.txn_id().is_some() {
            Some(std::time::Instant::now())
        } else {
            None
        };

        result
    }

    #[tracing::instrument(
        skip_all,
        fields(stmt_id = stmt_id, statement_kind = tracing::field::Empty)
    )]
    fn execute_impl(
        &mut self,
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

        let query_start = std::time::Instant::now();
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
        &mut self,
        session: &mut SessionState,
        statement: Statement,
    ) -> Result<ResultSet> {
        let (txn_id, autocommit) = self.txn_for_statement(session)?;
        let bound = Binder::new(&self.catalog).bind(statement).map_err(Error::from);
        let result = bound.and_then(|bound| self.execute_bound(bound, txn_id));

        if autocommit {
            match &result {
                Ok(_) => {
                    self.txn_manager.commit(txn_id, &self.buffer_pool)?;
                    self.maybe_checkpoint();
                }
                Err(_) => {
                    let _ = self.txn_manager.abort(txn_id, &self.buffer_pool);
                    let _ = self.reload_catalog();
                }
            }
        } else if result.is_err() {
            session.txn_slot = TxnSlot::Aborted(txn_id);
        }
        result
    }

    fn handle_begin(&mut self, session: &mut SessionState) -> Result<ResultSet> {
        if session.txn_slot.txn_id().is_some() {
            return Err(Error::NestedTransaction);
        }
        let isolation_level = IsolationLevel::ReadCommitted;
        let txn_id = self.txn_manager.begin(&self.buffer_pool, isolation_level)?;
        session.txn_slot = TxnSlot::Active(txn_id);
        session.txn_span = Some(
            tracing::info_span!("transaction", txn_id = txn_id.0, isolation = ?isolation_level),
        );
        self.current_txn = Some((session.session_id, txn_id));
        Ok(ResultSet::rows_affected(0))
    }

    fn handle_commit(&mut self, session: &mut SessionState) -> Result<ResultSet> {
        match session.txn_slot {
            TxnSlot::None | TxnSlot::TimedOut => {
                Err(Error::NoActiveTransaction { statement: "COMMIT".to_string() })
            }
            TxnSlot::Aborted(txn_id) => {
                self.txn_manager.abort(txn_id, &self.buffer_pool)?;
                self.reload_catalog()?;
                session.txn_slot = TxnSlot::None;
                session.txn_span = None;
                self.current_txn = None;
                self.maybe_checkpoint();
                Ok(ResultSet::RolledBack)
            }
            TxnSlot::Active(txn_id) => {
                self.txn_manager.commit(txn_id, &self.buffer_pool)?;
                session.txn_slot = TxnSlot::None;
                session.txn_span = None;
                self.current_txn = None;
                self.maybe_checkpoint();
                Ok(ResultSet::rows_affected(0))
            }
        }
    }

    fn handle_rollback(&mut self, session: &mut SessionState) -> Result<ResultSet> {
        let txn_id = session
            .txn_slot
            .txn_id()
            .ok_or_else(|| Error::NoActiveTransaction { statement: "ROLLBACK".to_string() })?;
        self.txn_manager.abort(txn_id, &self.buffer_pool)?;
        self.reload_catalog()?;

        session.txn_slot = TxnSlot::None;
        session.txn_span = None;
        self.current_txn = None;
        self.maybe_checkpoint();
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
        let reload_txn =
            self.txn_manager.begin(&self.buffer_pool, IsolationLevel::ReadCommitted)?;
        self.catalog = Catalog::open(&self.buffer_pool, reload_txn)?;
        self.txn_manager.commit(reload_txn, &self.buffer_pool)?;
        Ok(())
    }

    fn txn_for_statement(&mut self, session: &mut SessionState) -> Result<(TxnId, bool)> {
        match session.txn_slot {
            TxnSlot::Active(txn_id) => Ok((txn_id, false)),
            TxnSlot::Aborted(_) => Err(Error::TransactionAborted),
            TxnSlot::None | TxnSlot::TimedOut => {
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
