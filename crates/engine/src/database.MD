# database

A single open connection to a database. This used to own the catalog,
buffer pool, and transaction manager directly and run every statement
itself; as of M13.1 (`docs/ROADMAP.md`) it is a thin, ergonomic wrapper
around `runtime::EngineHandle`/`runtime::SessionHandle` (`runtime.MD`),
reached by message passing. A dedicated dispatch thread routes those
messages, but the statement itself runs on `runtime::worker_pool::WorkerPool`'s
fixed eight-thread pool (M10.2, `runtime.MD`/`worker_pool.MD`), so two
sessions' statements genuinely run at the same time rather than being
serialized behind one thread. `Database`'s own public surface - every
method's signature and observable behavior - is unchanged from before
the split: `cli`, both crash-injection harnesses, `flush_poisoning.rs`,
`database_lock.rs`, and every other `crates/engine/tests/*` file
construct and drive it exactly as they always have, with no idea a
dispatch thread or a worker pool exists underneath.

## Key Components

- `Database` - one field, `session: runtime::SessionHandle`, the one
  session this `Database` drives. Every method below is a thin forward
  onto `session`. There is no separate `_engine: Arc<runtime::EngineHandle>`
  field - `SessionHandle` itself holds that `Arc` (`runtime.MD`), so
  keeping the engine thread alive for as long as this `Database` exists
  falls out of `session` staying alive, with no second field and no
  drop-order relationship between fields to get right.
- `Database::open(config) -> Result<Self>` - the public entry point: opens
  a fresh `EngineHandle` (spawning its engine thread and running recovery
  synchronously first - see `runtime.MD`'s `EngineHandle::open`) and
  connects the one session this `Database` will use, and logs any error
  either step returns (`sql_state` and the error itself) exactly once
  before propagating it - the one place a startup failure is reported,
  per CLAUDE.md's "log at the boundary" rule. `EngineHandle::open` still
  opens `DiskManager` (which takes the database's one exclusive file
  lock) before the write-ahead log or the double-write buffer, and it
  still runs entirely on the calling thread before anything is handed to
  the engine thread, so a lock conflict or a corrupt file is still
  reported synchronously from this call, exactly as before the split -
  see `runtime.MD` for why that ordering has to be preserved.
- `Database::open_with_devices(config, db_device, wal_store,
  wal_segment_size, dwb_device) -> Result<Self>` - `#[cfg(any(test,
  feature = "test-util"))]`. Identical in shape to `open`, but through
  `EngineHandle::open_with_devices` against caller-supplied devices, for
  the crash-injection harness (`crash_injection.MD`) and
  `flush_poisoning.MD`.
- `Database::execute(&mut self, sql) -> Result<ResultSet>` - forwards to
  `SessionHandle::execute`, which sends the statement to the engine as an
  `EngineMessage::Execute` and blocks on its reply; the dispatch thread
  only routes that message, submitting the actual work to
  `worker_pool::WorkerPool` so a worker thread runs it (M10.2,
  `runtime.MD`/`worker_pool.MD`). Still takes `&mut self` even though
  nothing here is mutated directly anymore (all the state `execute` used
  to touch now lives in the engine's shared, thread-safe state
  (`EngineShared`), reached by whichever worker thread runs the
  statement) - changing the signature would violate this module's one
  hard rule, so the receiver stays `&mut` for a binding compatibility
  that no longer reflects an actual borrow need.
- `Database::execute_with_params(&mut self, sql, params) -> Result<ResultSet>` -
  forwards to `SessionHandle::execute_with_params`, sent as an
  `EngineMessage::ExecuteWithParams` next to `Execute` and dispatched onto
  the same worker pool. `runtime::EngineShared::execute` is now this
  method called with an empty `params` slice, so a bare `execute("SELECT
  ... WHERE a = $1")` with no values behaves exactly as it always has -
  the unsubstituted `$1` reaches `planner::Binder`, which rejects it as
  `common::Error::UndefinedParameter` (`42P02`) - while a non-empty
  `params` always goes through `planner::substitute_parameters` first, so
  a parameter-count mismatch is `common::Error::ParameterCountMismatch`
  (`08P01`) instead (`runtime.MD`'s `EngineShared::execute_impl`).
- `Database::describe(&self, sql) -> Result<StatementDescription>` -
  forwards to `SessionHandle::describe`, sent as an `EngineMessage::Describe`
  and dispatched onto the worker pool exactly like `execute`, so a
  `Describe` never blocks the dispatch thread either. Parses `sql`, infers
  each `$n` placeholder's type against the live catalog, binds the result
  after substituting a dummy value of each inferred type in place of every
  placeholder, and reads back a `SELECT`'s column names/types - starting
  no transaction and taking no lock, since it never executes a plan. See
  `statement_description.MD` and `runtime.MD`'s `EngineShared::describe`.
- `Database::close(self) -> Result<()>` - forwards to
  `SessionHandle::checkpoint_and_flush`: a final checkpoint, every
  write-ahead log record forced to disk, every dirty page flushed, and
  the file synced, run on a worker pool thread (the dispatch thread only
  submits the `Checkpoint` message's work) and awaited here so errors are
  still reported to the caller exactly as before - see `runtime.MD`'s
  `EngineState::checkpoint_and_flush`.
- `Database::connect(&self) -> Result<Self>` - opens a second session on
  the same engine via `SessionHandle::connect`, sharing the same
  `Arc<runtime::EngineHandle>` and therefore the same catalog, buffer pool,
  and transaction manager. This is how `crates/engine/tests/sessions.rs`
  drives more than one connection against one database without exposing
  `runtime::EngineHandle`/`SessionHandle` directly, and the shape M13.2's
  network listener needs to turn an accepted socket into a session.
- `Database::table_names(&self) -> Vec<String>`/`table_schema(&self, name)
  -> Result<Schema>` - forward to the same-named `SessionHandle` methods,
  which read the engine's live catalog through the same request/reply
  channel. `table_names` can't report an engine failure without changing
  its return type, so it collapses `SessionHandle::table_names`'s
  `Result<Vec<String>>` to an empty `Vec` on error rather than panicking or
  blocking forever - a real gap only if the engine has already died, which
  `execute`'s own `EngineUnavailable` would have surfaced first on any real
  workload.
- `Database` has **no `Drop` impl**, and that absence is deliberate. Its
  only field drops in the ordinary way, and `runtime::SessionHandle`'s own
  `Drop` sends `runtime::EngineMessage::Disconnect`, which is the whole of
  per-connection cleanup: the engine removes the session and a worker
  thread aborts whatever transaction it left open and reloads the catalog,
  with the dropping thread waiting for none of it. Until M13.2 this
  destructor also ran an engine-wide best-effort flush of the buffer pool,
  which was right while a process held one `Database` and dropped it once
  at exit, and wrong the moment `server` began calling `connect()` per
  accepted socket - every client hanging up then stalled the whole engine
  for a full pool flush. That flush now lives in `Drop for
  runtime::EngineHandle`, which runs exactly once, when the last handle in
  the process goes; see
  `docs/adr/0020-closing-a-connection-is-not-a-flush-point.md` for why
  nothing durable depended on the old placement, and `runtime.MD` for the
  mechanism. `close()` is unaffected and still checkpoints, flushes, syncs
  and returns the error, since an explicit close has somewhere to report a
  failure to and a destructor does not.
- `Database::kill_engine_for_test(&self)` - `#[cfg(any(test, feature =
  "test-util"))]`. Forwards to `SessionHandle::kill_engine_for_test`,
  sending `runtime::EngineMessage::Shutdown` to stop the engine thread
  without going through a real panic or process exit, so a test can assert
  every session's next request fails with `Error::EngineUnavailable`
  instead of hanging.
- `Database::stats(&self) -> Result<runtime::EngineStats>` - `#[cfg(any(test,
  feature = "test-util"))]`. Forwards to `SessionHandle::stats`, reading
  engine-wide counters (`checkpoints_written`, `engine_wide_flushes`,
  `version_chains` and `finished_txn_count`)
  straight off `EngineState` rather than through a tracing capture
  buffer, so a test asserting a behavioral invariant like "checkpoints
  don't multiply with sessions" or "whole-pool flushes don't multiply with
  sessions" (`crates/engine/tests/sessions.rs`),
  "the version store doesn't grow without bound"
  (`crates/engine/tests/version_store_growth.rs`), or "the lock manager's
  finished-transaction set doesn't grow without bound"
  (`crates/engine/tests/lock_manager_growth.rs`) does not depend on log
  wording or on tracing's thread-local dispatcher and process-wide
  callsite-interest cache both cooperating under concurrent test threads.
- `Database::current_txn_id(&self) -> Result<Option<common::TxnId>>` -
  `#[cfg(feature = "test-util")]`. Forwards to
  `SessionHandle::current_txn_id`, reading this session's own
  `TxnSlot::txn_id()` off the engine thread - `None` if this session has
  no open transaction. Lets a test capture the id of a transaction it is
  about to `COMMIT`/`ROLLBACK`, so it can still ask about that specific
  transaction's locks afterward via `lock_count_for_txn`, once
  `SessionState::txn_slot` itself no longer has any record of it.
- `Database::lock_count_for_txn(&self, txn_id) -> Result<usize>` -
  `#[cfg(feature = "test-util")]`. Forwards to
  `SessionHandle::lock_count_for_txn`, which reads
  `txn::LockManager::held_lock_count(txn_id)` (`txn::lock_manager.MD`)
  straight off the engine's shared lock manager - for asserting that
  `TransactionManager::commit`/`abort` actually released every lock a
  transaction held, rather than inferring it indirectly
  (`crates/engine/tests/sessions.rs`).

## Usage Example

```rust
use common::DbConfig;
use engine::Database;

let mut db = Database::open(DbConfig::new("example.db"))?;
db.execute("CREATE TABLE t (a INTEGER)")?;
db.execute("BEGIN")?;
db.execute("INSERT INTO t VALUES (1)")?;
db.execute("COMMIT")?;
db.close()?;
```

`cli` opens exactly one `Database` per process and calls `execute` once
per statement the REPL reads; `crates/engine/tests/*` drive the same
public API against real and fault-injecting devices to prove the
transaction, recovery, and checkpoint behavior `runtime.MD` documents
actually holds. Every one of them still only ever sees `Database` - the
dispatch thread, the worker pool, and the message-passing protocol
between them are entirely internal to this crate.
