# database

A single open connection to a database. This used to own the catalog,
buffer pool, and transaction manager directly and run every statement
itself; as of M13.1 (`docs/ROADMAP.md`) it is a thin, ergonomic wrapper
around `runtime::EngineHandle`/`runtime::SessionHandle` (`runtime.MD`),
which do the real work on one dedicated engine thread reached by message
passing. `Database`'s own public surface - every method's signature and
observable behavior - is unchanged from before the split: `cli`, both
crash-injection harnesses, `flush_poisoning.rs`, `database_lock.rs`, and
every other `crates/engine/tests/*` file construct and drive it exactly
as they always have, with no idea an engine thread exists underneath.

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
  `SessionHandle::execute`, which sends the statement to the engine
  thread and blocks on its reply. Still takes `&mut self` even though
  nothing here is mutated directly anymore (all the state `execute` used
  to touch now lives on the engine thread) - changing the signature would
  violate this module's one hard rule, so the receiver stays `&mut` for a
  binding compatibility that no longer reflects an actual borrow need.
- `Database::close(self) -> Result<()>` - forwards to
  `SessionHandle::checkpoint_and_flush`: a final checkpoint, every
  write-ahead log record forced to disk, every dirty page flushed, and
  the file synced, run on the engine thread and awaited here so errors
  are still reported to the caller exactly as before - see
  `runtime.MD`'s `EngineState::checkpoint_and_flush`.
- `Database::connect(&self) -> Result<Self>` - opens a second session on
  the same engine thread via `SessionHandle::connect`, sharing the same
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
- `impl Drop for Database` - forwards to `SessionHandle::best_effort_flush`,
  which blocks on the engine thread's reply so the flush this used to do
  synchronously (before any thread existed to hand it to) still completes
  before `Database::drop` returns. Skips the flush entirely once the pool
  is poisoned, and swallows any other error - there is nowhere to report
  one from `Drop` - matching the pre-split behavior exactly; see
  `runtime.MD`'s `EngineState::best_effort_flush` and
  `storage::buffer::BufferPool`'s poisoning paragraph (`buffer.MD`) for
  why a destructor must not attempt a second, uncoordinated flush once
  poisoning has already happened. `close()` is unaffected and still
  attempts its own flush and still returns the error, since an explicit
  close has somewhere to report a failure to and a destructor does not.
  Dropping `session` right after (an ordinary field drop, not this
  method) sends `runtime::EngineMessage::Disconnect` - see `runtime.MD`.
- `Database::kill_engine_for_test(&self)` - `#[cfg(any(test, feature =
  "test-util"))]`. Forwards to `SessionHandle::kill_engine_for_test`,
  sending `runtime::EngineMessage::Shutdown` to stop the engine thread
  without going through a real panic or process exit, so a test can assert
  every session's next request fails with `Error::EngineUnavailable`
  instead of hanging.
- `Database::stats(&self) -> Result<runtime::EngineStats>` - `#[cfg(any(test,
  feature = "test-util"))]`. Forwards to `SessionHandle::stats`, reading
  engine-wide counters (currently just `checkpoints_written`) straight off
  `EngineState` rather than through a tracing capture buffer, so a test
  asserting a behavioral invariant like "checkpoints don't multiply with
  sessions" (`crates/engine/tests/sessions.rs`) does not depend on log
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
engine thread and its message-passing protocol are entirely internal to
this crate.
