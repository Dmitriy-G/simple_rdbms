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

**What this file does not yet do.** Only the structural split and the
request/reply plumbing landed here: `EngineState`/`SessionState` are
correctly separated, and multiple sessions can already be connected
(via `EngineHandle::connect`) and interleaved on the engine thread
correctly for ordinary, independent statements. What is *not* here yet -
deliberately, as the next piece of `docs/ROADMAP.md`'s M14.1 entry - is
the admission control the design still needs once two sessions can
actually contend: a FIFO park queue so a second session's `BEGIN` (or an
autocommit statement, which is just as much "a second transaction" while
another session holds one open) waits instead of running concurrently
with an already-open explicit transaction; an idle-in-transaction timeout
so a session that `BEGIN`s and goes silent doesn't wedge every parked
request forever; and `SessionHandle::drop` triggering a rollback when a
parked request's session disappears mid-wait. `Disconnect` already
removes a session's state and is wired into `Drop for SessionHandle`, but
without a park queue yet, there's nothing in-flight for a disconnect to
rescue anyone from. Today, two sessions really can both hold an
explicit transaction open at once with nothing to stop it - a correctness
gap only until that admission-control layer lands, tracked as the very
next piece of this same milestone.

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
- `TxnSlot` - private. Unchanged from before the split: `None` (no
  `BEGIN` open - every statement runs autocommit), `Active(TxnId)` (an
  explicit transaction is open and healthy), or `Aborted(TxnId)` (open
  but a statement inside it already failed - every statement until
  `ROLLBACK` is rejected with `common::Error::TransactionAborted`).
  `TxnSlot::txn_id(self) -> Option<TxnId>` - private, unchanged.
- `SessionState` - private. Everything M14.1's split assigns
  per-connection: `txn_slot`, `txn_span: Option<tracing::Span>` (the
  current explicit transaction's span - `None` under autocommit),
  `conn_span: tracing::Span` (this session's own span - a plain `Span`,
  not an entered guard, since the engine thread services many sessions
  and can only ever have one span-stack entered at a time; see
  `EngineState::execute` for how it gets (re-)entered per request rather
  than held continuously the way `database.rs`'s old `_conn_span:
  EnteredSpan` field was), and `next_stmt_id` (per-connection, not
  process-wide, matching the pre-split field of the same name).
  `SessionState::new(conn_span)` - starts with `txn_slot: TxnSlot::None`,
  no `txn_span`, and `next_stmt_id: 1`.
- `EngineMessage` - private. Every request a `SessionHandle`/`EngineHandle`
  can send the engine thread, each carrying a one-shot
  `mpsc::SyncSender` for its reply where a reply makes sense:
  `Connect` (allocate a `SessionId` and register a fresh `SessionState`),
  `Execute` (run one statement for an existing session), `TableNames`/
  `TableSchema` (read the live catalog), `Checkpoint` (the engine-wide
  work `Database::close` needs), `BestEffortFlush` (the engine-wide work
  `Drop for Database` needs), and `Disconnect` (no reply - fire-and-forget
  session teardown, sent from `Drop for SessionHandle`).
- `engine_unavailable() -> common::Error` - private free function.
  Builds `Error::EngineUnavailable` with a fixed diagnostic detail message.
  Used everywhere a channel `send`/`recv` fails, which only happens if the
  engine thread has already exited (panicked, most likely, since nothing
  in this design ever deliberately exits it) - every such failure maps to
  this one variant rather than a hang or a generic error, per
  `common::error.MD`.
- `EngineHandle` - `pub(crate)`. Wraps the one
  `mpsc::SyncSender<EngineMessage>` every session's requests and every
  `connect()` call funnel through. `EngineHandle::open(config) ->
  Result<Arc<Self>>`/`open_with_devices(config, db_device, wal_store,
  wal_segment_size, dwb_device) -> Result<Arc<Self>>` - build the real
  (or caller-supplied, test-only) `DiskManager`/`DoubleWriteBuffer`/
  `LogManager` exactly as `database.rs`'s old `open_impl`/
  `open_with_devices` did, then call `spawn`. Both run entirely on the
  calling thread up through `EngineState::open` succeeding, before any
  thread exists - a lock conflict, a corrupt header, or an injected fault
  during bootstrap still surfaces as a synchronous `Err` from these
  functions, exactly as it did before the split, which is what lets
  `database_lock.rs` and every crash-injection test keep working
  unmodified. `EngineHandle::spawn(config, disk_manager, dwb, log_manager)
  -> Result<Arc<Self>>` - private. Runs `EngineState::open` (still on the
  calling thread), then spawns the engine thread carrying that state and
  the channel's receiving half, propagating the calling thread's tracing
  dispatcher (see above). The spawned closure captures only `state` and
  `receiver`, never a clone of `sender` - the engine thread must never
  hold a sender of its own, or the channel could never disconnect and the
  thread could never exit once every real handle drops.
  `EngineHandle::connect(&self) -> Result<SessionHandle>` - sends
  `Connect`, awaits the assigned `SessionId`, and returns a `SessionHandle`
  wrapping a clone of the same `mpsc::SyncSender` (cheap and
  `Send + Sync` - `std::sync::mpsc::SyncSender` is safe to call `send` on
  concurrently through a shared reference, which is what lets many
  sessions share one channel without a wrapping lock of their own) plus
  the new `SessionId`.
- `SessionHandle` - `pub(crate)`. `execute(&self, sql) ->
  Result<ResultSet>`/`table_schema(&self, name) -> Result<Schema>`/
  `checkpoint_and_flush(&self) -> Result<()>` all follow the same shape:
  build a one-shot reply channel, send the matching `EngineMessage`
  (mapping a `send` failure to `engine_unavailable()`), and block on the
  reply (mapping a `recv` failure the same way, then unwrapping the inner
  `Result` the engine sent back). `table_names(&self) -> Vec<String>`
  follows the same shape but can't report a channel failure without
  changing its return type, so it answers with an empty `Vec` instead -
  see `database.MD` for why that's an acceptable gap.
  `best_effort_flush(&self)` sends `BestEffortFlush` and waits for the
  reply, discarding whatever it gets (including a send/recv failure) -
  used only from `Drop for Database`, which has nowhere to report an
  error either way.
- `impl Drop for SessionHandle` - sends `Disconnect` for this session,
  ignoring a send failure (the engine is already gone, so there is
  nothing left to disconnect from). This is what actually removes the
  session's `SessionState` from the engine's map; without it, a session
  whose `Database` was dropped without calling `close()` would leak
  forever in `run_engine`'s `sessions` map.
- `run_engine(state, receiver)` - private free function, the engine
  thread's entire body. A single `for message in receiver` loop - which
  exits, ending the thread, exactly when every `EngineMessage` sender
  (every `SessionHandle` and the one inside `EngineHandle`) has been
  dropped, per ordinary `mpsc` channel-disconnection semantics; nothing
  here signals shutdown explicitly. Owns the `HashMap<SessionId,
  SessionState>` no other thread ever touches, so it needs no lock of its
  own. An `Execute` for a `session_id` the map no longer has (a request
  that raced a `Disconnect` for the same session - not reachable today
  since nothing runs concurrently with itself on one `SessionHandle`, but
  cheap to make correct anyway) answers `engine_unavailable()` rather
  than panicking on a missing key.
- `EngineState` - private. `catalog: Catalog`, `buffer_pool:
  Arc<BufferPool>`, `txn_manager: TransactionManager` (a plain owned
  field now, not `Arc<Mutex<TransactionManager>>` - the engine thread is
  its sole accessor by construction, so the `Mutex` the pre-split
  `Database` needed only to satisfy `Send + Sync` on a struct that could
  be handed across threads is no longer buying anything and was removed),
  `checkpoint_byte_threshold`, `bytes_at_last_checkpoint`, and
  `slow_query_warn_threshold_ms` - the five fields `task.MD`'s M14.1 plan
  calls "engine-wide", now genuinely shared across every session rather
  than duplicated per-`Database` the way `bytes_at_last_checkpoint` used
  to be (a real bug the old layout had: N `Database`s each tracked their
  own threshold and checkpointed N times as often against one log this
  split fixes as a side effect of where the field now lives, not as a
  separate change).
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
- `EngineState::checkpoint_and_flush(&mut self) -> Result<()>` - the work
  `Database::close` needs: `write_checkpoint`, `flush_log_all`,
  `flush_all`, `sync`, then `info!` "connection closed", in that order,
  unchanged from the pre-split `close`. Does not check
  `is_flush_poisoned` first - an explicit close should still attempt the
  flush and still return the real error, exactly as `task.MD`'s P-4 and
  `buffer.MD`'s poisoning paragraph require.
- `EngineState::best_effort_flush(&self)` - the work `Drop for Database`
  needs: skip entirely if `buffer_pool.is_flush_poisoned()`, otherwise
  attempt `flush_log_all`/`flush_all` and discard any error - unchanged
  from the pre-split `Drop for Database` body, just relocated so it can
  run on the engine thread where `buffer_pool` actually lives.
- `EngineState::maybe_checkpoint(&mut self)` - unchanged from the
  pre-split method of the same name: skips if poisoned, skips if the log
  hasn't grown `checkpoint_byte_threshold` bytes since the last
  checkpoint, otherwise checkpoints and logs a `warn!` on failure without
  propagating it (the triggering statement's own effects are unaffected).
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
  severity plus the full error at `debug` on failure).
- `EngineState::execute_impl`/`execute_non_control_statement`/
  `handle_begin`/`handle_commit`/`handle_rollback`/`handle_explain`/
  `reload_catalog`/`txn_for_statement`/`execute_bound`/`populate_index`/
  `run` - all unchanged in behavior from their pre-split, identically-named
  `Database` methods, each now taking `&mut self: &mut EngineState` and,
  where the pre-split method touched `txn_slot`/`txn_span`, an additional
  `session: &mut SessionState` parameter. `handle_explain` and
  `populate_index`/`execute_bound`/`run` need no `SessionState` at all,
  matching how `handle_explain` never touched `txn_slot`/`txn_span`
  before either.
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
that one session. Nothing outside `engine` ever names `EngineHandle`,
`SessionHandle`, or `EngineMessage` directly; they exist to let one
engine thread stand behind `Database`'s unchanged public API, not to be
a second public surface of their own - not yet, at least, until a future
milestone's wire-protocol frontend needs to `connect()` more than one
session against a single running engine.
