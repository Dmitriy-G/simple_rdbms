# database

A single open connection to a database. This used to own the catalog,
buffer pool, and transaction manager directly and run every statement
itself; as of M14.1 (`docs/ROADMAP.md`) it is a thin, ergonomic wrapper
around `runtime::EngineHandle`/`runtime::SessionHandle` (`runtime.MD`),
which do the real work on one dedicated engine thread reached by message
passing. `Database`'s own public surface - every method's signature and
observable behavior - is unchanged from before the split: `cli`, both
crash-injection harnesses, `flush_poisoning.rs`, `database_lock.rs`, and
every other `crates/engine/tests/*` file construct and drive it exactly
as they always have, with no idea an engine thread exists underneath.

## Key Components

- `Database` - `_engine: Arc<runtime::EngineHandle>` (kept alive for as
  long as this `Database` exists, but never called through directly -
  see `runtime.MD` for why a leading underscore is the right way to mark
  a field held only for its lifetime, the same convention the pre-split
  `_conn_span` field used) and `session: runtime::SessionHandle`, the one
  session this `Database` drives. Every method below is a thin
  forward onto `session`.
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
- `Database::table_names(&self) -> Vec<String>`/`table_schema(&self, name)
  -> Result<Schema>` - forward to the same-named `SessionHandle` methods,
  which read the engine's live catalog through the same request/reply
  channel. `table_names` can't report an engine failure without changing
  its return type, so it answers with an empty `Vec` in that case rather
  than panicking or blocking forever - a real gap only if the engine has
  already died, which `execute`'s own `EngineUnavailable` would have
  surfaced first on any real workload.
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
