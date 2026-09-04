# runtime

The database engine's own thread and the message-passing protocol that
reaches it: `EngineShared` (the engine-wide state - catalog, buffer pool,
transaction manager, checkpoint bookkeeping, each behind its own lock so
more than one thread can touch it safely) and `SessionState` (the
per-connection state - the explicit-transaction slot, its tracing span,
the per-connection statement counter). `EngineHandle::open`/
`open_with_devices` spawn one dedicated dispatch thread running
`run_engine`'s message loop, plus a fixed-size `worker_pool::WorkerPool`
(`worker_pool.MD`) that actually runs every statement; every `Database`
holds one `SessionHandle` and reaches the dispatch thread exclusively
through `EngineMessage`s sent over a bounded channel and replies awaited
on a matching one-shot reply channel per request. `database.MD` is the
outside view - `Database`'s own public methods are thin forwards onto a
`SessionHandle`.

**From one thread to a worker pool (M10.2).** Through M13.1, every
statement from every session ran on the single dispatch thread itself,
which gave serializable isolation by construction at the cost of
admission control rejecting a second session's attempt to hold an
explicit transaction concurrently (`common::Error::LockTimeout`, `55P03`)
and parking anything else behind whichever session held the one
transaction slot. `docs/ROADMAP.md`'s M10.2 entry removes that
restriction: `storage::buffer::BufferPool`, `catalog::Catalog`, and
`txn::TransactionManager` are all `Send + Sync`
(`crates/engine/tests/send_sync.rs`), and `EngineShared` now actually
shares them across threads rather than owning them exclusively on one -
`catalog: RwLock<Catalog>` (many concurrent readers - every `SELECT`/
`INSERT`'s bind and plan step - or one writer - `CREATE TABLE`/
`CREATE INDEX`, held for that statement's whole duration since neither is
performance-sensitive), `txn_manager: Mutex<TransactionManager>` (locked
only for the brief `begin`/`commit`/`abort`/`get` call itself, never
across a page fetch or an executor's `next()`), and
`checkpoint: Mutex<CheckpointState>` (the two checkpoint-bookkeeping
counters, locked only long enough to read or update them). Every lock
acquisition goes through `common::sync::recover_lock` rather than
`.unwrap()`/`.expect()`, so one panicking statement poisons a lock without
wedging every later, unrelated statement (`common::sync.MD`). The dispatch
thread's own loop (`run_engine`) still owns the `HashMap<SessionId,
Arc<Mutex<SessionState>>>` session table with no lock of its own - it is
the only thread that ever inserts or removes an entry - but each
individual `SessionState` is now reachable from whichever worker thread is
running that session's current statement, hence the `Arc<Mutex<_>>`
around it. `EngineMessage::Execute` no longer runs inline on the dispatch
thread: `dispatch_execute` looks the session up, clones the two `Arc`s it
needs, and calls `WorkerPool::submit` with a job that locks the session,
runs `EngineShared::execute`, and replies - the dispatch thread is free to
process the next message (a `Connect`, another `Execute` for a different
session, a tick) the instant `submit` returns, which is what actually lets
two sessions' statements run at the same time instead of merely being
*admitted* concurrently while still serialized on one thread.

**Idle-in-transaction timeout, now per session.** `SessionState::idle_since`
(`:62`) and `DbConfig::idle_in_transaction_timeout_ms` still drive
`expire_idle_transaction`, run once per `TICK_INTERVAL` from `tick`, but
with no single `current_txn` left to name "the" holder, it now walks every
entry in the session table: `session.try_lock()` (never a blocking
`lock()` - the dispatch thread must never block on a client-controlled
wait) skips a session whose worker is mid-statement, since that worker
will refresh `idle_since` itself the moment the statement finishes. A
poisoned session lock (a worker panicked mid-statement while holding it)
is recovered via `PoisonError::into_inner` inline, the same
`recover_lock` behavior, rather than silently skipped forever. Every
other session's `SessionState` is untouched by an expiry, proven by
`crates/engine/tests/sessions.rs`'s
`a_session_idle_in_transaction_does_not_affect_another_sessions_own_open_transaction`.

**Never block the dispatch thread on a lock a worker might hold (P-14).**
The `try_lock` above only ever *peeks* at whether a session looks overdue;
it never itself calls `abort_txn`/`reload_catalog`. Those calls -
`EngineShared::reload_catalog` takes `EngineShared::catalog`'s *write*
lock, which a worker running a long `SELECT`/`INSERT` holds for that
whole statement's `read` lock's duration (`run`, below) - used to run
inline on the dispatch thread in both `expire_idle_transaction` and
`disconnect`, which meant a slow statement on session A could make the
dispatch thread block for that statement's entire duration the moment
session B's idle timeout fired or session B disconnected, stalling every
other session's `Connect`/`Execute`/`TableNames`/etc. in the meantime -
exactly the failure mode the worker pool exists to remove, reintroduced
through this one code path. Both now only ever *submit* the actual
abort-and-reload work to `worker_pool::WorkerPool` - `expire_idle_session`
and `disconnect_session` (below) - the same way `dispatch_execute` already
does for `Execute`, so the dispatch thread never itself takes a lock a
worker might be holding; the submitted job re-checks the session's own
condition after actually acquiring its lock (blocking is fine there - it's
a worker thread, not the dispatch thread), since time has passed between
the cheap peek and the job actually running and the condition might no
longer hold (a statement may have completed and refreshed `idle_since` in
between, for instance). `crates/engine/tests/dispatch_never_blocks.rs`
(`dispatch_never_blocks.MD`) is the regression test: it pauses a `SELECT`
mid-scan with a device-level hook so it genuinely blocks rather than
merely runs slowly, disconnects a session with its own open transaction
while that pause holds `catalog`'s read lock, and asserts a fourth
session's `table_names()` still comes back quickly - disabling the fix
locally while writing that test reproduced an unbounded hang, not just a
slow response, confirming this was a real deadlock hazard rather than a
theoretical one.

**Propagating the tracing subscriber across thread boundaries.** Every
event this module logs - and everything logged by the storage/catalog/
executor layers underneath a statement it runs - used to be emitted on
whatever thread called `Database::execute`, so a test that installed a
capturing subscriber via `tracing::subscriber::set_default` (which is
thread-local, not global - `crates/engine/tests/logging.rs`) simply saw
it. `EngineHandle::spawn` captures the *calling* thread's current
dispatcher (`tracing::dispatcher::get_default`) at spawn time, wraps the
dispatch thread's entire body in `tracing::dispatcher::with_default`
against it, and passes a clone of the same captured dispatcher into
`WorkerPool::new`, which does the same for every one of its worker
threads (`worker_pool.MD`) - every thread this module ever spawns emits
through the exact subscriber that was active when `Database::open` was
called, not just the dispatch thread. `logging.rs`'s tests still pass
unmodified because they call `set_default` before `Database::open`, which
is what spawns every one of these threads; had they called it after, or
had this propagation been left out for even one of the worker threads,
every test whose statement actually executed on a worker (which, since
M10.2, is every test) would have silently seen zero captured events
instead of failing loudly.

## Key Components

- `SessionId` - private, `Copy`. An opaque per-connection identifier,
  handed out by `NEXT_SESSION_ID` (a process-wide `AtomicU64`) and
  doubling as the tracing `conn_id` field, since both mean the same
  thing: the Nth connection opened this process.
- `TxnSlot` - private. `None` (no `BEGIN` open - every statement runs
  autocommit), `Active(TxnId)` (an explicit transaction is open and
  healthy), `Aborted(TxnId)` (open but a statement inside it already
  failed - every statement until `ROLLBACK` is rejected with
  `common::Error::TransactionAborted`), or `TimedOut` (this session's own
  transaction was aborted by `expire_idle_transaction`; distinct from
  `None` only so the *next* statement this session sends can report
  `common::Error::IdleInTransactionTimeout` once before the slot resets to
  `None` - see `EngineShared::execute_impl`).
  `TxnSlot::txn_id(self) -> Option<TxnId>` - private; `None` and
  `TimedOut` both answer `None`, `Active`/`Aborted` both answer `Some`.
- `SessionState` - private. Everything per-connection: `session_id` (a
  copy of this session's own key), `txn_slot`, `txn_span:
  Option<tracing::Span>` (the current explicit transaction's span - `None`
  under autocommit), `conn_span: tracing::Span` (this session's own span -
  a plain `Span`, not an entered guard, since more than one worker thread
  services sessions over time and none can hold a span-stack entered
  continuously; see `EngineShared::execute` for how it gets (re-)entered
  per request), `next_stmt_id` (per-connection, not process-wide), and
  `idle_since: Option<Instant>` (when this session's current explicit
  transaction last went quiet - `Some` only while `txn_slot` holds an
  active transaction and no statement is in flight; set at the end of
  every `EngineShared::execute` call, read by `expire_idle_transaction`).
  `SessionState::new(session_id, conn_span)` - starts with `txn_slot:
  TxnSlot::None`, no `txn_span`, `next_stmt_id: 1`, and no `idle_since`.
  Every live `SessionState` is reached through an `Arc<Mutex<SessionState>>`
  in the dispatch thread's session table, since a worker thread running
  this session's current statement needs its own handle to it.
- `EngineMessage` - private. Every request a `SessionHandle`/`EngineHandle`
  can send the dispatch thread, each carrying a one-shot
  `mpsc::SyncSender` for its reply where a reply makes sense:
  `Connect` (allocate a `SessionId` and register a fresh `SessionState`),
  `Execute` (submit one statement for an existing session to the worker
  pool - see `dispatch_execute` below), `TableNames`/`TableSchema` (read
  the live catalog), `Checkpoint` (the engine-wide work `Database::close`
  needs), `BestEffortFlush` (the engine-wide work `Drop for Database`
  needs), `Disconnect` (no reply - fire-and-forget session teardown, sent
  from `Drop for SessionHandle`), and `Shutdown` (`#[cfg(feature =
  "test-util")]` - stops the dispatch thread on request, for
  `a_dead_engine_thread_fails_every_session_with_a_fatal_error` in
  `tests/sessions.MD`), `Stats` (`#[cfg(feature = "test-util")]` - reads
  `EngineShared::stats()` back to the caller, for a test to assert a
  behavioral invariant like "checkpoints don't multiply with sessions"
  against a real counter instead of grepping captured log text),
  `CurrentTxnId` (`#[cfg(feature = "test-util")]` - reads this session's
  own `TxnSlot::txn_id()` straight off the session table, handled inline
  in `dispatch_message` rather than submitted to the worker pool, since a
  `HashMap` lookup and a `try_lock`-free `recover_lock` never blocks on
  anything client-controlled), and `LockCountForTxn` (`#[cfg(feature =
  "test-util")]` - reads `EngineShared::lock_count_for(txn_id)`, for a
  test to ask how many locks a specific, possibly already-finished
  transaction still holds).
- `engine_unavailable() -> common::Error` - private free function.
  Builds `Error::EngineUnavailable`; used everywhere a channel
  `send`/`recv` fails, which only happens if the dispatch thread has
  already exited (panicked, or told to stop via `Shutdown`).
- `unknown_session(session_id) -> common::Error` - private free function.
  Builds `Error::UnknownSession`; `dispatch_execute` returns this instead
  of panicking or submitting a job for a request that names a `SessionId`
  the session table no longer has.
- `EngineHandle` - `pub(crate)`. `sender: Option<mpsc::SyncSender<EngineMessage>>`
  plus `thread: Option<std::thread::JoinHandle<()>>`, both `Option` purely
  so `Drop` can `take()` them - every live `EngineHandle` always has both
  populated. `EngineHandle::send(&self, message) -> Result<()>` - private,
  the one place that maps a missing/closed sender to `engine_unavailable()`.
  `EngineHandle::open(config) -> Result<Arc<Self>>`/`open_with_devices(config,
  db_device, wal_store, wal_segment_size, dwb_device) -> Result<Arc<Self>>` -
  build the real (or caller-supplied, test-only) `DiskManager`/
  `DoubleWriteBuffer`/`LogManager`, then call `spawn`. Both run entirely on
  the calling thread up through `EngineShared::open` succeeding, before any
  thread exists - a lock conflict, a corrupt header, or an injected fault
  during bootstrap still surfaces as a synchronous `Err` from these
  functions. `EngineHandle::spawn(config, disk_manager, dwb, log_manager)
  -> Result<Arc<Self>>` - private. Runs `EngineShared::open` (still on the
  calling thread), captures the calling thread's tracing dispatcher, builds
  a `worker_pool::WorkerPool` sized `WORKER_POOL_SIZE` with a clone of that
  dispatcher, then spawns the dispatch thread carrying an `EngineState`
  (the shared state plus the worker pool) and the channel's receiving
  half, wrapped in the same dispatcher. The spawned closure captures only
  `state` and `receiver`, never a clone of `sender` - the dispatch thread
  must never hold a sender of its own, or the channel could never
  disconnect and the thread could never exit once every real handle drops.
  `EngineHandle::connect(self: &Arc<Self>) -> Result<SessionHandle>` -
  sends `Connect`, awaits the assigned `SessionId`, and returns a
  `SessionHandle` wrapping `Arc::clone(self)` plus the new `SessionId`.
- `impl Drop for EngineHandle` - `take()`s `sender`, dropping the last
  live `mpsc::SyncSender<EngineMessage>` unless the dispatch thread's own
  clone-free receiver loop already saw every session's sender go away
  first, then `take()`s and joins `thread`. This runs precisely when the
  last `Arc<EngineHandle>` - which by construction means the last
  `SessionHandle` - drops, so the join can never hang waiting on a session
  that still exists; the join blocks until `run_engine` returns, which
  drops its `EngineState` and therefore the `WorkerPool` inside it, which
  itself joins every worker thread before `run_engine` can return - so
  `EngineHandle`'s own join transitively waits out every worker too,
  guaranteeing `EngineShared` (and the `DiskManager`/exclusive file lock it
  owns) is gone before `Database::drop` returns.
- `SessionHandle` - `pub(crate)`. `engine: Arc<EngineHandle>` (the sole
  thing keeping the dispatch thread alive on this session's behalf) plus
  `session_id`. `connect(&self) -> Result<SessionHandle>` - forwards to
  `self.engine.connect()`, opening a second, independent session on the
  same dispatch thread - `database.MD`'s `Database::connect` is the public
  door onto this. `execute(&self, sql) -> Result<ResultSet>`/
  `table_schema(&self, name) -> Result<Schema>`/`checkpoint_and_flush(&self)
  -> Result<()>`/`table_names(&self) -> Result<Vec<String>>` all follow the
  same shape: build a one-shot reply channel, send the matching
  `EngineMessage` through `self.engine.send`, and block on the reply.
  `best_effort_flush(&self)` sends `BestEffortFlush` and waits for the
  reply, discarding whatever it gets - used only from `Drop for Database`.
  `kill_engine_for_test(&self)`/`stats(&self) -> Result<EngineStats>`/
  `current_txn_id(&self) -> Result<Option<TxnId>>`/`lock_count_for_txn(&self,
  txn_id) -> Result<usize>` - `#[cfg(feature = "test-util")]`, test-only.
- `impl Drop for SessionHandle` - sends `Disconnect` for this session
  through `self.engine.send`, ignoring a failure. This is what actually
  removes the session's `SessionState` from the session table; without
  it, a session whose `Database` was dropped without calling `close()`
  would leak forever. `mpsc` delivers everything queued before a
  channel's last sender disconnects, so the dispatch thread always sees
  this session's `Disconnect` before the receive loop ends.
- `EngineState` - private. `shared: Arc<EngineShared>` plus `worker_pool:
  worker_pool::WorkerPool` - the two things the dispatch thread's own
  loop needs and nothing more; every actual piece of engine state lives on
  `EngineShared` instead, reachable by any worker thread through its own
  `Arc` clone.
- `run_engine(state, receiver)` - private free function, the dispatch
  thread's entire body. Owns the `HashMap<SessionId, Arc<Mutex<SessionState>>>`
  session table no other thread ever inserts into or removes from, so it
  needs no lock of its own even though the *values* it holds are shared.
  The loop is `receiver.recv_timeout(TICK_INTERVAL)` (50ms), not a
  blocking `for message in receiver`: a message dispatches through
  `dispatch_message`, a timeout runs `tick`, and a `Disconnected` error
  (every `EngineMessage` sender has been dropped) ends the loop, ending
  the thread, which drops `state` (and therefore the `WorkerPool` inside
  it - see `impl Drop for EngineHandle` above).
- `dispatch_message(state, sessions, message) -> bool` - private free
  function. One arm per `EngineMessage` variant; returns `false` only for
  `Shutdown`. `Execute` goes through `dispatch_execute` rather than
  running inline, and `Disconnect` goes through the free function
  `disconnect` (below) rather than a bare `sessions.remove`, since it
  still has an open transaction to abort if one exists. `CurrentTxnId`
  and `LockCountForTxn` (both `#[cfg(feature = "test-util")]`) answer
  inline too, for the same reason `TableNames`/`TableSchema` do: a
  `HashMap`/lock-manager lookup can never block on anything a client
  controls, so there is nothing a worker-pool job would buy here.
- `tick(state, sessions)` - private free function, run once per
  `TICK_INTERVAL` with no message pending. Calls `expire_idle_transaction`;
  unlike the pre-M10.2 version there is no waiter queue left to expire or
  admit.
- `dispatch_execute(state, sessions, session_id, sql, reply)` - private
  free function, `dispatch_message`'s handler for `Execute`. Replies
  `unknown_session(session_id)` immediately if the session is gone;
  otherwise clones the session's `Arc<Mutex<SessionState>>` and
  `state.shared`'s `Arc`, and calls `state.worker_pool.submit` with a job
  that locks the session (via `common::sync::recover_lock`), calls
  `EngineShared::execute`, and sends the result to `reply` - this function
  itself never blocks and never runs a statement; it only ever builds and
  submits the job.
- `expire_idle_transaction(state, sessions)` - private free function, run
  from `tick`. For every session in the table: `try_lock`s it (skipping
  one a worker is currently using, recovering one poisoned by a panicked
  worker via `PoisonError::into_inner`) just long enough to *peek* at
  whether it holds a transaction whose `idle_since` has passed
  `EngineShared::idle_in_transaction_timeout` - this peek never mutates
  anything and never calls `abort_txn`/`reload_catalog` itself (see the
  P-14 file-level note above for why). If overdue, clones the session's
  `Arc`/`state.shared`'s `Arc` and submits a job running
  `expire_idle_session` to `state.worker_pool` - this function itself
  never blocks past the cheap `try_lock`.
- `expire_idle_session(shared, session, session_id)` - private free
  function, the job body `expire_idle_transaction` submits. Locks the
  session for real (blocking is fine here - a worker thread, not the
  dispatch thread) and re-checks the same condition the peek already
  checked, since a statement may have run for this session and refreshed
  `idle_since` in the time between the peek and this job actually
  running; only if it's still overdue does it abort the session's
  transaction, reload the catalog, set `txn_slot` to `TxnSlot::TimedOut`,
  clear `txn_span`/`idle_since`, and log a `warn!` naming the session and
  transaction. Every other session's `SessionState` is never touched by
  either function, which is the entire fix M10.2 needed over the
  single-`current_txn` version this replaces (see the file-level note
  above).
- `disconnect(state, sessions, session_id)` - private free function,
  `dispatch_message`'s handler for `Disconnect`. Removes the session from
  `sessions` first (nothing else can reach it once removed - a session
  gone from the table can never be looked up by a later `Execute`, so
  there is no race to guard against there), then, if a session with that
  id existed, clones `state.shared`'s `Arc` and submits a job running
  `disconnect_session` to `state.worker_pool` rather than doing the abort
  and catalog reload inline (P-14, see the file-level note above) - this
  function itself never blocks.
- `disconnect_session(shared, session, session_id)` - private free
  function, the job body `disconnect` submits. Locks the session (again,
  blocking is fine on a worker thread) and, if it held an `Active`/
  `Aborted` transaction, aborts it, reloads the catalog, and logs a
  `warn!` - the session left connected mid-transaction case.
- `CheckpointState` - private. `bytes_at_last_checkpoint`/
  `checkpoints_written`, the two checkpoint counters, grouped into one
  struct so a single `Mutex` covers both without them drifting apart under
  concurrent commits from different sessions.
- `EngineShared` - private. `catalog: RwLock<Catalog>`, `buffer_pool:
  Arc<BufferPool>`, `txn_manager: Mutex<TransactionManager>`,
  `checkpoint: Mutex<CheckpointState>`, `checkpoint_byte_threshold`,
  `slow_query_warn_threshold_ms`, and `idle_in_transaction_timeout` - see
  the file-level note above for why each mutable field is behind its own
  lock rather than requiring `&mut self` the way the pre-M10.2 `EngineState`
  did. `EngineShared::open(config, disk_manager, dwb, log_manager) ->
  Result<Self>` - repairs a torn page from the double-write buffer, stands
  up the buffer pool, runs ARIES recovery, bootstraps or loads the catalog
  under its own committed transaction, and emits the `info!` "database
  opened" event, identical in behavior to the pre-split `open_with_managers`.
- `EngineShared::record_checkpoint_written(&self)` - locks `checkpoint`,
  increments `checkpoints_written` and
  `metrics::counter!("checkpoints_written_total")` together.
  `stats(&self) -> EngineStats` - `#[cfg(feature = "test-util")]`. Copies
  the current counter. `lock_count_for(&self, txn_id) -> usize` -
  `#[cfg(feature = "test-util")]`. Locks `txn_manager` just long enough to
  call `txn::TransactionManager::lock_manager().held_lock_count(txn_id)` -
  test-only introspection into how many locks a transaction (commonly one
  a test already committed or aborted) still holds.
  `checkpoint_and_flush(&self) -> Result<()>` - the work `Database::close`
  needs: `write_checkpoint` (under a brief `txn_manager` lock),
  `record_checkpoint_written`, `flush_log_all`, `flush_all`, `sync`, in
  that order. `best_effort_flush(&self)` - the work `Drop for Database`
  needs: skip entirely if `buffer_pool.is_flush_poisoned()`, otherwise
  attempt `flush_log_all`/`flush_all` and discard any error.
  `maybe_checkpoint(&self)` - skips if poisoned, skips if the log hasn't
  grown `checkpoint_byte_threshold` bytes since the last checkpoint
  (read under a brief `checkpoint` lock), otherwise checkpoints, calls
  `record_checkpoint_written` on success, and logs a `warn!` on failure
  without propagating it. Two statements on different sessions can, in
  principle, both observe the threshold crossed and both checkpoint
  before either updates `bytes_at_last_checkpoint`; the result is at most
  one redundant checkpoint, never a missed one, so this is left as is
  rather than serialized further.
- `EngineShared::table_names(&self) -> Vec<String>`/`table_schema(&self,
  name) -> Result<Schema>` - each takes a brief `catalog` read lock.
- `EngineShared::execute(&self, session, sql) -> Result<ResultSet>` - the
  per-request entry point a worker-pool job calls for every `Execute`
  message, with the session already locked by that job. Unchanged in
  behavior from before M10.2 split statement execution off the dispatch
  thread: clones `session.conn_span`/`session.txn_span` into locals before
  entering either (entering directly off `session.conn_span.enter()` would
  hold an immutable borrow of `session` across the later `&mut session`
  the dispatch call needs), enters both for the duration of the call,
  checks `buffer_pool.is_flush_poisoned()` before dispatching to
  `execute_impl`, and logs the same success/failure events at the same
  levels as before. After logging, sets `session.idle_since` to
  `Some(Instant::now())` if `session.txn_slot` still holds a transaction
  id or `None` otherwise - the only place `idle_since` is written, so
  `expire_idle_transaction`'s reading of it always reflects time since the
  *last* statement, not time since `BEGIN`.
- `EngineShared::execute_impl` - starts by checking whether
  `session.txn_slot` is `TxnSlot::TimedOut`: if so, resets it to `None`
  (clearing `txn_span` too) and returns `Error::IdleInTransactionTimeout`
  immediately - this is how a session finds out its transaction was
  aborted out from under it by `expire_idle_transaction`, reported exactly
  once, on the first statement it sends after the fact. Otherwise lexes,
  parses, and dispatches on statement kind to `handle_begin`/
  `handle_commit`/`handle_rollback`/`handle_explain`/
  `execute_non_control_statement`.
- `EngineShared::execute_non_control_statement(&self, session, statement)
  -> Result<ResultSet>` - resolves this statement's transaction
  (`txn_for_statement`), binds it under a brief `catalog` read lock, and
  dispatches to `execute_bound`; on autocommit, commits (propagating a
  commit failure) or aborts-and-reloads (swallowing that failure, since
  the statement's own error is what the caller sees) depending on the
  result, otherwise marks the session's `txn_slot` `Aborted` on failure so
  every later statement in the same explicit transaction is rejected until
  `ROLLBACK`.
- `EngineShared::handle_begin`/`handle_commit`/`handle_rollback` -
  identical in behavior to the pre-M10.2 methods of the same name, minus
  the `current_txn` bookkeeping those used to maintain (removed entirely -
  there is no longer one engine-wide "the" transaction to name).
  `handle_begin` locks `txn_manager` only for the `begin` call itself.
- `EngineShared::handle_explain(&self, statement) -> Result<ResultSet>` -
  takes one `catalog` read lock spanning bind, optimize, and both
  `explain_logical`/`explain_physical` calls, since all three need the
  same borrowed `&Catalog`. Never begins a transaction and never touches
  the buffer pool beyond the catalog already resident in memory.
- `EngineShared::reload_catalog(&self) -> Result<()>` - begins and commits
  a short-lived bootstrap transaction, calls `Catalog::open` against it
  outside any lock (this can take a while - it replays the whole catalog
  heap), then swaps the result into `self.catalog` under a single brief
  write lock. Called after every abort/rollback, since `Catalog::create_table`/
  `create_index` mutate the in-memory catalog immediately rather than
  waiting for commit, and an aborted transaction needs that mutation
  discarded.
- `EngineShared::commit_txn(&self, txn_id) -> Result<()>`/`abort_txn(&self,
  txn_id) -> Result<()>` - private one-line wrappers that lock
  `txn_manager` just long enough for the underlying `TransactionManager`
  call; every commit/abort site in this file goes through one of these
  two rather than locking `txn_manager` itself, so the lock is never held
  any longer than the single call needs.
- `EngineShared::txn_for_statement(&self, session) -> Result<(TxnId, bool)>` -
  unchanged in behavior: returns the session's already-open transaction
  (not autocommit) or begins a fresh one under a brief `txn_manager` lock
  (autocommit) depending on `session.txn_slot`.
- `EngineShared::execute_bound(&self, bound, txn_id) -> Result<ResultSet>` -
  matches on `BoundStatement`. `CreateTable`/`CreateIndex` each take a
  `catalog` *write* lock spanning the whole mutation (`create_index` plus
  `populate_index`, in `CreateIndex`'s case, since populating an index is
  rare enough that serializing readers against it for that span costs
  nothing); `Insert`/`Select` never take a write lock, only the read locks
  `run`/the `Select` arm's `Optimizer::optimize` call need.
- `EngineShared::populate_index(&self, catalog, txn_id, table_id,
  column_index, index_id) -> Result<()>` - takes `&Catalog` directly
  (the caller's already-held write guard derefs to it) rather than
  reaching through `self.catalog` itself, since every method it calls
  (`get_table_by_id`, `index_root_page`, `update_index_root_page`) only
  ever needed `&Catalog` in the first place.
- `EngineShared::run(&self, physical, txn_id) -> Result<Vec<Tuple>>` -
  clones the transaction and an `Arc<txn::LockManager>` clone out of
  `txn_manager` (a brief lock; `txn::Transaction` is cheap `Clone` data
  with no ties back to the manager, and cloning the `Arc` is just a
  refcount bump, so together they're the *only* borrow the executor ever
  needs - this is what keeps a possibly-long-running scan from holding
  `txn_manager` for its whole duration), then takes a `catalog` read lock
  spanning `ExecutorContext` construction and the entire `init`/`next`
  loop, since `ExecutorContext` borrows `&Catalog` for as long as the
  executor runs. The same shared `LockManager` every transaction against
  this database locks and unlocks through is what lets one session's
  `SeqScanExecutor`/`IndexScanExecutor`/`InsertExecutor` (`executor`'s own
  `seq_scan.MD`/`index_scan.MD`/`insert.MD`) block behind another
  session's conflicting table or row lock, taken through the same
  instance.
- `new_connection_span(session_id) -> tracing::Span` - private free
  function. Creates an `info_span!("connection", conn_id)`, enters it just
  long enough to log `info!` "connection opened", then exits and returns
  the plain (not entered) `Span` for `SessionState` to store.
- `statement_kind(&Statement) -> &'static str`/`syntax_error(err, sql) ->
  Error` - private free functions, unchanged.

## Usage Example

```rust
let engine = EngineHandle::open(&config)?;
let session = engine.connect()?;
let result = session.execute("SELECT * FROM t")?;
session.checkpoint_and_flush()?;
```

`database.rs`'s `Database` is the only caller of any of this - `open`/
`open_with_devices` build one `EngineHandle` and `connect` one
`SessionHandle`, and every other `Database` method forwards straight to
that one session, `Database::connect` included: it calls
`SessionHandle::connect` to open a second `Database` sharing the same
dispatch thread and worker pool, which is how
`crates/engine/tests/sessions.rs` (`tests/sessions.MD`) drives more than
one session - now genuinely concurrent ones - against one database.
Nothing outside `engine` ever names `EngineHandle`, `SessionHandle`,
`EngineMessage`, or `worker_pool::WorkerPool` directly; they exist to let
one dispatch thread and its worker pool stand behind `Database`'s public
API, not to be a second public surface of their own.
