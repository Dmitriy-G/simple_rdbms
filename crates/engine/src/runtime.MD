# runtime

The database engine's own thread and the message-passing protocol that
reaches it: `EngineState` (the engine-wide state - catalog, buffer pool,
transaction manager, checkpoint bookkeeping) and `SessionState` (the
per-connection state - the explicit-transaction slot, its tracing span,
the per-connection statement counter) that M14.1 (`docs/ROADMAP.md`)
splits out of what used to be one `Database` struct doing everything on
whichever thread called it. `EngineHandle::open`/`open_with_devices` spawn
one dedicated OS thread running `run_engine`'s message loop; every
`Database` holds one `SessionHandle` and reaches that thread exclusively
through `EngineMessage`s sent over a bounded channel and replies awaited
on a matching one-shot reply channel per request. `database.MD` is the
outside view - `Database`'s own public methods are thin forwards onto a
`SessionHandle` - this file is where the actual statement-execution logic
that used to live directly on `Database` now lives, unchanged in
behavior, just relocated and threaded through an explicit `SessionState`
parameter everywhere it used to read `self` directly.

**Why a single dedicated thread, not a lock.** `docs/ROADMAP.md`'s M14
entry (as corrected - see its "Dependency correction" paragraph) is
explicit that this is a design choice, not something the storage layer
forces: `storage::buffer::BufferPool`, `catalog::Catalog`, and
`txn::TransactionManager` are all already `Send + Sync`
(`crates/engine/tests/send_sync.rs`) and could in principle be shared
directly across real connection threads behind a lock or a
`txn::LockManager`/MVCC scheme. Running everything through one thread
instead makes every statement from every session execute serially in
arrival order, which gives serializable isolation by construction - no
lock manager, no MVCC, nothing to get subtly wrong - at the cost of one
transaction open at a time across the whole engine. M10, when it lands,
is what would let a future milestone relax this back into genuine
multi-threaded execution.

**Admission control: at most one explicit transaction at a time.** With
`EngineState`/`SessionState` split and multiple sessions able to connect,
the remaining piece of `docs/ROADMAP.md`'s M14.1 entry was making that
actually safe: nothing before stopped two sessions from both holding an
explicit transaction open at once, racing writes against the same
buffer pool with no lock manager and no MVCC. `EngineState::current_txn:
Option<(SessionId, TxnId)>` now names the one session, if any, allowed to
be mid-explicit-transaction; `handle_begin` records the holder there and
`handle_commit`/`handle_rollback`/`expire_idle_transaction`/`disconnect`
all clear it. A second session's `BEGIN` - or an autocommit statement,
which opens and closes its own transaction just as much as `BEGIN` does -
does not run concurrently with the holder: `must_park`/
`peek_needs_txn_slot` decide, by lexing and parsing just enough of the
statement to know whether it needs a fresh transaction, whether the
request has to wait; `admit_execute` parks a request that does into
`waiters`, a FIFO `VecDeque<ParkedRequest>`, instead of executing it. A
parked request is granted the moment the holder commits or rolls back
(`try_admit_waiters`, called right after both), and fails with
`common::Error::LockTimeout` (`55P03`) if `DbConfig::lock_wait_timeout_ms`
elapses first (`expire_waiters`, driven by `tick`). A holder that goes
silent mid-transaction is aborted on its own after
`DbConfig::idle_in_transaction_timeout_ms`
(`expire_idle_transaction`, also driven by `tick`), and a session that
disconnects mid-transaction is rolled back rather than abandoned
(`disconnect`) - both matter for the same underlying reason: either one
left unhandled would pin `TransactionManager::earliest_active_begin_lsn`
forever and mean the write-ahead log can never be truncated again
(`docs/adr/0011-segmented-write-ahead-log.md`), on top of blocking every
other session's transactions permanently once only one can be open at a
time. `run_engine`'s loop switched from a blocking `for message in
receiver` to `receiver.recv_timeout(TICK_INTERVAL)` specifically so `tick`
has somewhere to run on a schedule even when no session is sending
anything - the engine thread must never block waiting on a client, since
one idle connection would otherwise stall every other session's
statements along with it.

**Propagating the tracing subscriber across the thread boundary.** Every
event this module logs - and everything logged by the storage/catalog/
executor layers underneath a statement it runs - used to be emitted on
whatever thread called `Database::execute`, so a test that installed a
capturing subscriber via `tracing::subscriber::set_default` (which is
thread-local, not global - `crates/engine/tests/logging.rs`) simply saw
it. Once statements run on a separate, dedicated engine thread, that
thread has no subscriber of its own by default and would silently drop
every event instead of routing it to whatever the caller installed.
`EngineHandle::spawn` fixes this by capturing the *calling* thread's
current dispatcher (`tracing::dispatcher::get_default`) at spawn time and
wrapping the entire engine thread body in
`tracing::dispatcher::with_default` against that captured dispatcher, so
the engine thread emits through the exact subscriber that was active when
`Database::open` was called. `logging.rs`'s tests still pass unmodified
because they call `set_default` before `Database::open`, which is what
spawns the thread - had they called it after, or had this propagation
been left out, every one of those tests would have silently seen zero
captured events instead of failing loudly, which is exactly the kind of
mistake worth naming here rather than leaving for the next person to
rediscover by watching a test go quiet.

## Key Components

- `SessionId` - private, `Copy`. An opaque per-connection identifier,
  handed out by `NEXT_SESSION_ID` (a process-wide `AtomicU64`, the same
  counter role `database.rs`'s old `NEXT_CONN_ID` played) and doubling as
  the tracing `conn_id` field, since both mean the same thing: the Nth
  connection opened this process.
- `TxnSlot` - private. `None` (no `BEGIN` open - every statement runs
  autocommit), `Active(TxnId)` (an explicit transaction is open and
  healthy), `Aborted(TxnId)` (open but a statement inside it already
  failed - every statement until `ROLLBACK` is rejected with
  `common::Error::TransactionAborted`), or `TimedOut` (the engine aborted
  this session's transaction on its own via `expire_idle_transaction`;
  distinct from `None` only so the *next* statement this session sends can
  report `common::Error::IdleInTransactionTimeout` once before the slot
  resets to `None` - see `EngineState::execute_impl`).
  `TxnSlot::txn_id(self) -> Option<TxnId>` - private; `None` and
  `TimedOut` both answer `None`, `Active`/`Aborted` both answer
  `Some`.
- `SessionState` - private. Everything M14.1's split assigns
  per-connection: `session_id` (a copy of this session's own key, needed
  so `EngineState::handle_begin` can record `(SessionId, TxnId)` into
  `EngineState::current_txn` without the caller passing it separately),
  `txn_slot`, `txn_span: Option<tracing::Span>` (the current explicit
  transaction's span - `None` under autocommit), `conn_span:
  tracing::Span` (this session's own span - a plain `Span`, not an
  entered guard, since the engine thread services many sessions and can
  only ever have one span-stack entered at a time; see
  `EngineState::execute` for how it gets (re-)entered per request rather
  than held continuously the way `database.rs`'s old `_conn_span:
  EnteredSpan` field was), `next_stmt_id` (per-connection, not
  process-wide, matching the pre-split field of the same name), and
  `idle_since: Option<Instant>` (when this session's current explicit
  transaction last went quiet - `Some` only while `txn_slot` holds an
  active transaction and no statement is in flight; set at the end of
  every `EngineState::execute` call, read by `expire_idle_transaction`).
  `SessionState::new(session_id, conn_span)` - starts with `txn_slot:
  TxnSlot::None`, no `txn_span`, `next_stmt_id: 1`, and no `idle_since`.
- `ParkedRequest` - private. One `Execute` request `admit_execute` decided
  had to wait: `session_id`, the `sql` text (re-parsed once more when the
  request is finally admitted, rather than caching the parsed
  `Statement` - parking is expected to be rare enough that re-parsing
  isn't worth carrying a second representation), the one-shot `reply`
  channel, and `deadline` (`Instant::now() + EngineState::lock_wait_timeout`
  at the moment it was parked).
- `EngineMessage` - private. Every request a `SessionHandle`/`EngineHandle`
  can send the engine thread, each carrying a one-shot
  `mpsc::SyncSender` for its reply where a reply makes sense:
  `Connect` (allocate a `SessionId` and register a fresh `SessionState`),
  `Execute` (run one statement for an existing session, subject to
  admission control - see above), `TableNames`/`TableSchema` (read the
  live catalog), `Checkpoint` (the engine-wide work `Database::close`
  needs), `BestEffortFlush` (the engine-wide work `Drop for Database`
  needs), `Disconnect` (no reply - fire-and-forget session teardown, sent
  from `Drop for SessionHandle`), and `Shutdown` (`#[cfg(any(test,
  feature = "test-util"))]` - stops the engine thread on request, for
  `a_dead_engine_thread_fails_every_session_with_a_fatal_error` in
  `tests/sessions.MD` to prove every session's next request reports
  `EngineUnavailable` instead of hanging, without needing to actually
  panic the thread to produce a dead one), and `Stats` (`#[cfg(any(test,
  feature = "test-util"))]` - reads `EngineState::stats()` back to the
  caller, for a test to assert a behavioral invariant like "checkpoints
  don't multiply with sessions" against a real counter instead of
  grepping captured log text, which depends on tracing's thread-local
  dispatcher and process-wide callsite-interest cache both behaving under
  concurrent test threads - see `EngineState::checkpoints_written` below).
- `engine_unavailable() -> common::Error` - private free function.
  Builds `Error::EngineUnavailable` with a fixed diagnostic detail message.
  Used everywhere a channel `send`/`recv` fails, which only happens if the
  engine thread has already exited (panicked, or told to stop via
  `Shutdown`) - every such failure maps to this one variant rather than a
  hang or a generic error, per `common::error.MD`.
- `unknown_session(session_id) -> common::Error` - private free function.
  Builds `Error::UnknownSession`; `admit_execute` returns this instead of
  panicking or blocking forever when a request names a `SessionId` the
  engine's session map no longer has (the session disconnected between
  sending the request and the engine thread reading it - not reachable
  from `SessionHandle`'s own API today since a session only ever sends
  through its own handle, but cheap to make correct anyway, the same
  reasoning `run_engine`'s pre-split ancestor already applied to a missing
  key on `Execute`).
- `lock_timeout() -> common::Error` - private free function. Builds
  `Error::LockTimeout` with a fixed diagnostic detail message; both
  `try_admit_waiters` and `expire_waiters` use it for a parked request
  whose deadline passed before the holder freed the slot.
- `EngineHandle` - `pub(crate)`. `sender: Option<mpsc::SyncSender<EngineMessage>>`
  plus `thread: Option<std::thread::JoinHandle<()>>`, both `Option` purely
  so `Drop` can `take()` them (see below) - every live `EngineHandle`
  always has both populated. `EngineHandle::send(&self, message) ->
  Result<()>` - private. The one place that maps a missing/closed sender
  to `engine_unavailable()`; every other method funnels its `send` through
  this instead of touching `self.sender` directly.
  `EngineHandle::open(config) -> Result<Arc<Self>>`/`open_with_devices(config,
  db_device, wal_store, wal_segment_size, dwb_device) -> Result<Arc<Self>>` -
  build the real (or caller-supplied, test-only) `DiskManager`/
  `DoubleWriteBuffer`/`LogManager` exactly as `database.rs`'s old
  `open_impl`/`open_with_devices` did, then call `spawn`. Both run
  entirely on the calling thread up through `EngineState::open`
  succeeding, before any thread exists - a lock conflict, a corrupt
  header, or an injected fault during bootstrap still surfaces as a
  synchronous `Err` from these functions, exactly as it did before the
  split, which is what lets `database_lock.rs` and every crash-injection
  test keep working unmodified. `EngineHandle::spawn(config, disk_manager,
  dwb, log_manager) -> Result<Arc<Self>>` - private. Runs `EngineState::open`
  (still on the calling thread), then spawns the engine thread carrying
  that state and the channel's receiving half, propagating the calling
  thread's tracing dispatcher (see above). The spawned closure captures
  only `state` and `receiver`, never a clone of `sender` - the engine
  thread must never hold a sender of its own, or the channel could never
  disconnect and the thread could never exit once every real handle
  drops. `EngineHandle::connect(self: &Arc<Self>) -> Result<SessionHandle>` -
  sends `Connect`, awaits the assigned `SessionId`, and returns a
  `SessionHandle` wrapping `Arc::clone(self)` (not a bare clone of the
  sender - see below) plus the new `SessionId`.
- `impl Drop for EngineHandle` - `take()`s `sender`, dropping the last
  live `mpsc::SyncSender<EngineMessage>` unless the engine thread's own
  clone-free receiver loop already saw every session's sender go away
  first, then `take()`s and joins `thread`. This runs precisely when the
  last `Arc<EngineHandle>` - which by construction means the last
  `SessionHandle`, since that is the only place one is stored - drops, so
  the join can never hang waiting on a session that still exists, and it
  is this join, not merely dropping a channel, that guarantees
  `EngineState` (and the `DiskManager`/exclusive file lock it owns) is
  gone before `Database::drop` returns. See `database.MD` for why the
  previous shape - `Database` holding `_engine: Arc<EngineHandle>` and
  `session: SessionHandle` as sibling fields - let `Database::drop` return
  before the engine thread had actually exited.
- `SessionHandle` - `pub(crate)`. `engine: Arc<EngineHandle>` (the sole
  thing keeping the engine thread alive on this session's behalf - see
  `impl Drop for EngineHandle` above) plus `session_id`.
  `connect(&self) -> Result<SessionHandle>` - forwards to
  `self.engine.connect()`, opening a second, independent session on the
  same engine thread - `database.MD`'s `Database::connect` is the public
  door onto this. `execute(&self, sql) -> Result<ResultSet>`/
  `table_schema(&self, name) -> Result<Schema>`/`checkpoint_and_flush(&self)
  -> Result<()>`/`table_names(&self) -> Result<Vec<String>>` all follow the
  same shape: build a one-shot reply channel, send the matching
  `EngineMessage` through `self.engine.send` (which already maps a closed
  channel to `engine_unavailable()`), and block on the reply (mapping a
  `recv` failure the same way). `table_names` used to swallow a channel
  failure into an empty `Vec` itself; that collapse now happens one layer
  up, in `database.MD`'s `Database::table_names`, so this method can
  report the real error to any future caller that wants it.
  `best_effort_flush(&self)` sends `BestEffortFlush` and waits for the
  reply, discarding whatever it gets (including a send failure) - used
  only from `Drop for Database`, which has nowhere to report an error
  either way. `kill_engine_for_test(&self)` - `#[cfg(any(test, feature =
  "test-util"))]`. Sends `Shutdown` and ignores the result; test-only.
  `stats(&self) -> Result<EngineStats>` - `#[cfg(any(test, feature =
  "test-util"))]`. Sends `Stats` and blocks on the reply, the same
  request/reply shape as `table_names`/`table_schema`; test-only, since
  production code has no need to read engine counters back out.
- `impl Drop for SessionHandle` - sends `Disconnect` for this session
  through `self.engine.send`, ignoring a failure (the engine is already
  gone, so there is nothing left to disconnect from). This is what
  actually removes the session's `SessionState` from the engine's map;
  without it, a session whose `Database` was dropped without calling
  `close()` would leak forever in `run_engine`'s `sessions` map. This
  `Disconnect` is queued before `self.engine` (the `Arc<EngineHandle>`)
  itself drops on the very next line of an ordinary field drop, and `mpsc`
  delivers everything queued before a channel's last sender disconnects,
  so the engine thread always sees this session's `Disconnect` before the
  receive loop ends.

  **Why this send is a blocking `send`, not `try_send`.** `EngineHandle::send`
  calls `mpsc::SyncSender::send`, which blocks if `REQUEST_CHANNEL_CAPACITY`
  (64) outstanding messages are already queued. A full queue plus a busy
  engine therefore makes dropping a session block, and - since `EngineHandle`'s
  own `Drop` joins the engine thread (see above) - dropping the last
  `Database` sharing that engine blocks behind it too. This is a deliberate
  choice, not an oversight: `run_engine`'s loop never blocks waiting on a
  client (`recv_timeout(TICK_INTERVAL)`, see the file-level note above), so
  the queue always drains and this can only ever be a brief wait, never a
  deadlock; switching to `try_send` would trade that brief wait for a
  silent message loss on a full queue, and `Disconnect` has no reply
  channel for a caller to notice the loss or retry - the dropped session's
  `SessionState` would leak in `sessions` forever, which is worse than
  blocking. This stops being the right tradeoff once M14.2 turns
  connections into tokio tasks: a blocking `send` inside a `Drop` running
  on an async runtime's worker thread stalls that worker rather than just
  the one dropping thread, and the fix at that point needs to be an async
  disconnect path, not `try_send` (which would still lose the message).
- `run_engine(state, receiver)` - private free function, the engine
  thread's entire body. Owns the `HashMap<SessionId, SessionState>` and
  the `VecDeque<ParkedRequest>` waiter queue no other thread ever touches,
  so it needs no lock of its own. The loop is
  `receiver.recv_timeout(TICK_INTERVAL)` (50ms), not a blocking
  `for message in receiver`: a message dispatches through
  `dispatch_message`, a timeout runs `tick`, and a `Disconnected` error
  (every `EngineMessage` sender - every `SessionHandle` and the one
  inside `EngineHandle` - has been dropped) ends the loop, ending the
  thread. `recv_timeout` rather than a plain blocking `recv` is what lets
  `tick` run on a schedule even when no session sends anything, which
  admission control needs (see above) - the engine thread must never
  block waiting on a client.
- `dispatch_message(state, sessions, waiters, message) -> bool` - private
  free function. One arm per `EngineMessage` variant; returns `false`
  only for `Shutdown`, which `run_engine` treats exactly like a channel
  disconnecting - `Execute` goes through `admit_execute` rather than
  calling `EngineState::execute` directly, and `Disconnect` goes through
  the free function `disconnect` (below) rather than a bare
  `sessions.remove`, since both now have park-queue and `current_txn`
  bookkeeping to do that a bare removal used to skip.
- `tick(state, sessions, waiters)` - private free function, run once per
  `TICK_INTERVAL` with no message pending. In order: `expire_waiters`
  (fail every parked request whose deadline has passed),
  `expire_idle_transaction` (abort the current holder's transaction if
  it's been idle past `EngineState::idle_in_transaction_timeout`), then
  `try_admit_waiters` (grant the front waiter if the slot is now free -
  catches both the case where `expire_idle_transaction` just freed it and
  the case where a `COMMIT`/`ROLLBACK` freed it between ticks without
  itself having anyone left to admit).
- `peek_needs_txn_slot(sql, session) -> bool` - private free function.
  `false` immediately if `session` already holds `EngineState::current_txn`
  (its own statements never park behind themselves); otherwise lexes and
  parses `sql` far enough to know its `Statement` kind, answering `false`
  on a parse failure (a malformed statement doesn't need a transaction
  slot - it needs to fail with its own syntax error, immediately, not sit
  in the park queue until a timeout manufactures a different one) and
  `false` for `Commit`/`Rollback`/`Explain` (none of which opens a new
  transaction - a session with no transaction of its own sending one of
  these is headed straight for `NoActiveTransaction` or a plan explanation,
  neither of which contends with the current holder). Every other
  statement - `Begin` included - answers `true`: autocommit statements
  are just as much "a second transaction" as an explicit `BEGIN` while
  another session holds one open, so they park too.
- `must_park(current_txn, session_id, session, sql) -> bool` - private
  free function. `false` whenever `current_txn` is empty or already
  belongs to `session_id`; otherwise defers to `peek_needs_txn_slot`.
- `admit_execute(state, sessions, waiters, session_id, sql, reply)` -
  private free function, `dispatch_message`'s handler for `Execute`.
  Replies `unknown_session(session_id)` immediately if the session is
  gone; otherwise calls `must_park`, and either pushes a `ParkedRequest`
  onto the back of `waiters` (deadline = now + `lock_wait_timeout`) or
  runs `EngineState::execute` immediately and replies, then calls
  `try_admit_waiters` either way a slot might have just closed behind it
  (a statement that itself opened or closed the transaction slot can free
  the front waiter in the same call).
- `try_admit_waiters(state, sessions, waiters)` - private free function.
  While `current_txn` is empty and `waiters` is non-empty: drop and fail
  (`lock_timeout()`) any waiter already past its deadline, otherwise pop
  the front waiter, re-run its `sql` through `EngineState::execute`, and
  reply - looping so more than one now-eligible waiter can be admitted (or
  expired) in a single call, e.g. after `expire_idle_transaction` frees
  the slot with several requests already parked behind it. A waiter whose
  session disconnected while parked (removed from `sessions` but not yet
  pruned from `waiters` - `disconnect` prunes eagerly, so this is a
  defensive `continue`, not an expected path) is skipped rather than
  panicking on a missing key.
- `expire_waiters(waiters)` - private free function. Pops and fails
  every waiter at the front of the queue whose deadline has passed;
  `waiters` is FIFO and ordered by park time, not by deadline directly,
  but every waiter shares the same `lock_wait_timeout` so park order and
  deadline order coincide.
- `expire_idle_transaction(state, sessions)` - private free function.
  No-op unless `current_txn` is set, its session is still in `sessions`,
  and that session has an `idle_since` older than
  `EngineState::idle_in_transaction_timeout`. Otherwise: aborts the
  transaction (logging at `warn` rather than propagating - there is no
  caller to propagate to), reloads the catalog the same way a rollback
  does, clears `current_txn`, sets the session's `txn_slot` to
  `TxnSlot::TimedOut` (see above for why not `None` directly), clears
  `txn_span`/`idle_since`, and logs a `warn!` naming the session and
  transaction - an operator-visible event, the same class as a normal
  transaction abort.
- `disconnect(state, sessions, waiters, session_id)` - private free
  function, `dispatch_message`'s handler for `Disconnect`. First prunes
  every `waiters` entry belonging to `session_id` (a request this session
  can no longer receive a reply for - dropping the `reply` sender is
  harmless, but leaving the entry would eventually fail its own deadline
  for no reason and could wrongly block `try_admit_waiters` behind a
  waiter nothing is listening for). Then, if the session held an
  `Active`/`Aborted` transaction, aborts it, reloads the catalog, clears
  `current_txn`, and logs a `warn!` - the fix for the failure mode where a
  disconnect used to abandon the transaction forever, pinning
  `earliest_active_begin_lsn` and the WAL truncation bound behind it
  indefinitely. Finally removes the session from `sessions` and calls
  `try_admit_waiters`, since freeing the slot here (unlike a `COMMIT`)
  has no statement-completion path of its own to do it.
- `EngineStats` - `pub`, `#[cfg(feature = "test-util")]`,
  `Debug + Clone + Copy + Default`. A snapshot of engine-wide counters a
  test can assert on without going through tracing at all -
  `checkpoints_written: u64` today, read via `EngineState::stats()`.
- `EngineState` - private. `catalog: Catalog`, `buffer_pool:
  Arc<BufferPool>`, `txn_manager: TransactionManager` (a plain owned
  field now, not `Arc<Mutex<TransactionManager>>` - the engine thread is
  its sole accessor by construction, so the `Mutex` the pre-split
  `Database` needed only to satisfy `Send + Sync` on a struct that could
  be handed across threads is no longer buying anything and was removed),
  `checkpoint_byte_threshold`, `bytes_at_last_checkpoint`,
  `checkpoints_written` (a running count of every checkpoint that has
  actually completed, incremented by `record_checkpoint_written` and
  never reset - the field `EngineStats`/`stats()` below reads), and
  `slow_query_warn_threshold_ms` - the five fields `task.MD`'s M14.1 plan
  calls "engine-wide", now genuinely shared across every session rather
  than duplicated per-`Database` the way `bytes_at_last_checkpoint` used
  to be (a real bug the old layout had: N `Database`s each tracked their
  own threshold and checkpointed N times as often against one log this
  split fixes as a side effect of where the field now lives, not as a
  separate change). Plus three fields for admission control:
  `current_txn: Option<(SessionId, TxnId)>` (the one session, if any,
  allowed to be mid-explicit-transaction - see above),
  `lock_wait_timeout: Duration` (from `DbConfig::lock_wait_timeout_ms`,
  how long a parked request waits before failing with `LockTimeout`), and
  `idle_in_transaction_timeout: Duration` (from
  `DbConfig::idle_in_transaction_timeout_ms`, how long the holder may sit
  idle before `expire_idle_transaction` aborts it on its own).
- `EngineState::open(config, disk_manager, dwb, log_manager) ->
  Result<Self>` - identical to the pre-split `open_with_managers`: repairs
  a torn page from the double-write buffer, stands up the buffer pool,
  runs ARIES recovery, bootstraps or loads the catalog under its own
  committed transaction, and emits the `info!` "database opened" event.
  No longer takes or stores a connection span - "database opened"
  describes the *engine* starting, not any one session connecting to it,
  so it is no longer nested under a `conn_span` the way it used to be
  when opening the engine and opening its one connection were the same
  event; `new_connection_span` (below) is what logs "connection opened"
  now, separately, once per `Connect`.
- `EngineState::record_checkpoint_written(&mut self)` - private. Called
  from every place a checkpoint actually completes -
  `checkpoint_and_flush` and `maybe_checkpoint`'s `Ok` arm, never on a
  failed attempt. Increments `checkpoints_written` and
  `metrics::counter!("checkpoints_written_total")` together, so the two
  can never drift apart.
- `EngineState::stats(&self) -> EngineStats` - `#[cfg(any(test, feature =
  "test-util"))]`. Copies the current counters into an `EngineStats`;
  `EngineMessage::Stats`'s handler is the only caller.
- `EngineState::checkpoint_and_flush(&mut self) -> Result<()>` - the work
  `Database::close` needs: `write_checkpoint`, `flush_log_all`,
  `flush_all`, `sync`, in that order. No longer logs `info!` "connection
  closed" itself - that line described one connection closing back when
  `Database::close` and this method were the same call; now that it also
  runs as the engine-wide `Checkpoint` message any session can trigger,
  logging it here would mislabel an engine-level checkpoint as a
  connection event. Does not check `is_flush_poisoned` first - an
  explicit close should still attempt the flush and still return the real
  error, exactly as `task.MD`'s P-4 and `buffer.MD`'s poisoning paragraph
  require.
- `EngineState::best_effort_flush(&self)` - the work `Drop for Database`
  needs: skip entirely if `buffer_pool.is_flush_poisoned()`, otherwise
  attempt `flush_log_all`/`flush_all` and discard any error - unchanged
  from the pre-split `Drop for Database` body, just relocated so it can
  run on the engine thread where `buffer_pool` actually lives.
- `EngineState::maybe_checkpoint(&mut self)` - unchanged from the
  pre-split method of the same name: skips if poisoned, skips if the log
  hasn't grown `checkpoint_byte_threshold` bytes since the last
  checkpoint, otherwise checkpoints, calls `record_checkpoint_written` on
  success, and logs a `warn!` on failure without propagating it (the
  triggering statement's own effects are unaffected).
- `EngineState::table_names(&self) -> Vec<String>`/`table_schema(&self,
  name) -> Result<Schema>` - unchanged, read `self.catalog` directly.
- `EngineState::execute(&mut self, session, sql) -> Result<ResultSet>` -
  the per-request entry point `run_engine` calls for every `Execute`
  message; unchanged in behavior from the pre-split `Database::execute`,
  restructured to take `session: &mut SessionState` instead of reading
  `self` directly. Clones `session.conn_span` and `session.txn_span`
  into locals *before* entering either (entering directly off
  `session.conn_span.enter()` would hold an immutable borrow of `session`
  across the later `&mut session` the dispatch call needs - `Span` is a
  cheap handle to clone, the same reason the pre-split code already
  cloned `txn_span` rather than borrowing it), enters both for the
  duration of the call so a statement nests connection-span-then-
  (optionally) transaction-span the same way it always has, checks
  `buffer_pool.is_flush_poisoned()` before dispatching to `execute_impl`,
  and logs the same success/failure events at the same levels as before
  (see `database.MD`'s old text, preserved here rather than duplicated:
  `fingerprint`-only on success, `redacted()` at `warn`/`error` by
  severity plus the full error at `debug` on failure). After logging, sets
  `session.idle_since` to `Some(Instant::now())` if `session.txn_slot`
  still holds a transaction id (an explicit transaction is open and this
  was the statement that just finished, so the clock on its idle time
  restarts now) or `None` otherwise (autocommit, or the transaction just
  closed) - the only place `idle_since` is written, so
  `expire_idle_transaction`'s reading of it always reflects time since the
  *last* statement, not time since `BEGIN`.
- `EngineState::execute_impl` - unchanged from the pre-split method in
  its statement dispatch, but now starts by checking whether
  `session.txn_slot` is `TxnSlot::TimedOut`: if so, resets it to `None`
  (clearing `txn_span` too) and returns `Error::IdleInTransactionTimeout`
  immediately without touching the statement at all - this is how a
  session finds out its transaction was aborted out from under it by
  `expire_idle_transaction`, reported exactly once, on the first
  statement it sends after the fact, the same way `TransactionAborted`
  reports a transaction that failed for a different reason.
- `EngineState::execute_non_control_statement`/`handle_explain`/
  `reload_catalog`/`txn_for_statement`/`execute_bound`/`populate_index`/
  `run` - all unchanged in behavior from their pre-split, identically-named
  `Database` methods (`txn_for_statement`'s match on `session.txn_slot`
  now also covers `TxnSlot::TimedOut`, treated exactly like `None`: begin
  a fresh autocommit transaction - reachable only if `execute_impl`'s
  timed-out check above didn't already intercept the statement, which it
  always does, so this arm exists for exhaustiveness rather than because
  it's ever actually taken), each taking `&mut self: &mut EngineState`
  and, where the pre-split method touched `txn_slot`/`txn_span`, an
  additional `session: &mut SessionState` parameter. `handle_explain` and
  `populate_index`/`execute_bound`/`run` need no `SessionState` at all,
  matching how `handle_explain` never touched `txn_slot`/`txn_span`
  before either.
- `EngineState::handle_begin`/`handle_commit`/`handle_rollback` -
  unchanged from the pre-split methods in every respect but one: each now
  also maintains `current_txn` to match `session.txn_slot`.
  `handle_begin` sets `current_txn = Some((session.session_id, txn_id))`
  right after opening the transaction; `handle_commit`'s `Aborted` and
  `Active` arms and `handle_rollback` all set `current_txn = None` right
  after clearing `txn_slot`/`txn_span`. This is the write side of the
  admission-control invariant `must_park`/`try_admit_waiters` read from
  the other side - see the file-level note above.
- `new_connection_span(session_id) -> tracing::Span` - private free
  function, the replacement for the pre-split `new_connection_span()`.
  Creates an `info_span!("connection", conn_id)`, enters it just long
  enough to log `info!` "connection opened" nested under it, then exits
  and returns the plain (not entered) `Span` for `SessionState` to store -
  unlike the old version, this can't return an already-entered guard,
  since the engine thread cannot leave any one session's span
  permanently entered while it services every other session too.
- `statement_kind(&Statement) -> &'static str`/`syntax_error(err, sql) ->
  Error` - private free functions, unchanged from `database.rs`.

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
engine thread, which is how `crates/engine/tests/sessions.rs`
(`tests/sessions.MD`) drives more than one session against one database
today, and the shape M14.2's wire-protocol listener will use to turn each
accepted connection into a session of its own. Nothing outside `engine`
ever names `EngineHandle`, `SessionHandle`, or `EngineMessage` directly;
they exist to let one engine thread stand behind `Database`'s public API,
not to be a second public surface of their own.
