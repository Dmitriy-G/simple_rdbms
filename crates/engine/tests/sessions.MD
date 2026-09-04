# sessions

Integration coverage for M10.2's concurrent execution model
(`docs/ROADMAP.md`'s M10.2 entry, `crates/engine/src/runtime.MD`,
`crates/engine/src/worker_pool.MD`): several sessions may each hold their
own open transaction at the same time, statements from different sessions
actually run concurrently on the worker pool rather than one at a time,
the idle-in-transaction timeout is tracked per session rather than
engine-wide, and a disconnect or an idle timeout releases that session's
transaction rather than abandoning it. Every test here drives more than
one session against one database through `Database::connect`
(`crates/engine/src/database.MD`), which nothing under
`crates/engine/tests/` needed before this file - `logging.rs` and
everything else exercises exactly one connection.

## Key Components

- `CONCURRENT_TEST_TIMEOUT` - the bound every genuinely concurrent test in
  this file waits for a worker thread to report back through
  `recv_within`, so a regression that makes the worker pool hang fails
  fast with a clear message instead of hanging the test run.
- `recv_within(rx, timeout, what) -> T` - private helper, the same shape
  as `crates/storage/tests/buffer_pool_flush_races.rs`'s helper of the
  same name: blocks on `rx.recv_timeout(timeout)` and panics naming
  `what` if it elapses instead of returning, so a concurrent test that
  goes wrong fails with a message instead of a bare timeout.
- `row_count(ResultSet) -> usize` - private helper. Unwraps a `SELECT`'s
  row count, panicking with the actual variant's name for any other
  `ResultSet` shape, so an assertion failure names what it got instead of
  panicking inside a bare `match` with no message.
- `two_sessions_hold_open_transactions_at_the_same_time_and_both_commit` -
  `db1` and `db2` both `BEGIN`, both `INSERT`, and both `COMMIT`, with
  `db2`'s `BEGIN` sent while `db1`'s transaction is still open; both
  inserts are visible afterward. Proves the admission-control restriction
  M10.2 removes is actually gone: two explicit transactions really can be
  open across two sessions at once.
- `a_second_session_can_begin_immediately_without_55p03` - the narrower
  regression check for the same removal: a second session's `BEGIN`
  against a database with another session's transaction already open must
  neither park nor fail with `common::Error::LockTimeout` (`55P03`) - it
  must simply succeed.
- `inserts_from_eight_sessions_concurrently_all_land` - connects eight
  sessions, spawns one OS thread per session, and has every thread send
  its own `INSERT` at the same time, synchronizing completion through an
  `mpsc` channel each thread signals on before this test joins every
  handle and asserts on the propagated result; a final `SELECT` from the
  original session counts eight rows. This is the test that actually
  exercises `worker_pool::WorkerPool`: with `runtime.rs`'s single
  dispatch thread now only submitting jobs rather than running them, this
  is what proves statements from different sessions really execute at
  the same time rather than merely being *admitted* concurrently while
  still serialized on one thread.
- `dropping_a_session_mid_transaction_rolls_it_back` - `db2` begins,
  inserts, and is dropped without `COMMIT`/`ROLLBACK`; the row is not
  visible afterward through `db1` - `disconnect`'s abort path.
- `open_file(path)`/`wal_base_path(dir) -> PathBuf` - private helpers for
  the one test below that opens `Database::open_with_devices` directly
  (`database.MD`) against real files it can also inspect independently
  through a second `FileSegmentStore` handle on the same path.
- `dropping_a_session_mid_transaction_releases_the_wal_truncation_bound` -
  the regression test for the WAL-truncation half of the disconnect fix,
  and the most valuable test in this file: a small `buffer_pool_size` (4
  frames) forces the buffer pool to evict and flush the log well before
  it would otherwise, so twenty large-valued inserts inside `db2`'s still-
  open transaction reliably roll the WAL into more than one segment while
  the transaction is held; dropping `db2` and then driving enough further
  log growth through `db1` (`checkpoint_byte_threshold: 512` keeps
  triggering checkpoints) shows the earliest surviving segment number
  advance past where it was pinned while `db2`'s transaction was still
  open - proof that `disconnect` actually clears
  `TransactionManager::earliest_active_begin_lsn`'s contribution from the
  abandoned transaction, not just that the transaction's writes are
  undone.
- `an_idle_in_transaction_session_is_aborted_after_the_timeout` - a
  100ms `idle_in_transaction_timeout_ms`, a `BEGIN` with no statement
  after it, then a sleep past that timeout: the next statement reports
  `Error::IdleInTransactionTimeout` (`25P03`) exactly once, and a fresh
  `BEGIN`/`COMMIT` afterward succeeds normally - `expire_idle_transaction`
  plus `TxnSlot::TimedOut`'s one-shot report, both from `runtime.MD`.
- `a_session_idle_in_transaction_does_not_affect_another_sessions_own_open_transaction` -
  the test M10.2 needed that M10.1 could not have written: `db1` and
  `db2` both `BEGIN`, then `db2` sends a `SELECT` every 60ms for six
  rounds (360ms total) while `db1` sends nothing, against a 150ms
  timeout. `db1`'s next statement reports `IdleInTransactionTimeout` -
  it really did sit idle past the threshold - while `db2`, whose own
  `idle_since` kept resetting every round, still commits its own insert
  successfully afterward and that row is visible. This is what proves
  `expire_idle_transaction` tracks idleness *per session* rather than
  engine-wide: the single-`current_txn`-holder version this replaced
  could not have told these two apart, since only one session could ever
  hold a transaction at all.
- `a_dead_engine_thread_fails_every_session_with_a_fatal_error` -
  `Database::kill_engine_for_test` (`#[cfg(any(test, feature =
  "test-util"))]`, `database.MD`) stops the engine thread without a real
  panic; both `db1` and the independently-connected `db2` report
  `Error::EngineUnavailable` on their next statement rather than hanging,
  proving every session's channel fails the same way once the thread is
  gone, regardless of which session triggered the shutdown.
- `checkpoint_count(threshold, drive, statements) -> Result<u64>` -
  private helper. Opens a `Database`, creates a table, runs `drive`
  against it, and reads `Database::stats()?.checkpoints_written`
  (`database.MD`, `#[cfg(feature = "test-util")]`) - a real
  counter on the engine rather than a count of "checkpoint complete" log
  events, which would race tracing's process-wide callsite-interest cache
  under this file's now-genuinely-concurrent tests.
- `several_sessions_checkpoint_once_per_threshold_not_once_each` - runs
  the same forty `INSERT`s twice with the same `checkpoint_byte_threshold`:
  once through one session, once split alternately across two sessions
  connected to the same `Database`. Asserts the single-session run
  produces at least one checkpoint (the growth was large enough to prove
  something), and that the split run produces the *same* count, not
  double - regression coverage for the bug where each `Database` used to
  track `bytes_at_last_checkpoint` itself and N connections against one
  log checkpointed N times as often; `EngineShared` being the one place
  that field lives now (behind its own `Mutex<CheckpointState>`,
  `runtime.MD`) is what keeps this test passing even with several
  sessions' statements actually running at once.

## Usage Example

```rust
use common::DbConfig;
use engine::Database;

let dir = tempfile::tempdir()?;
let mut db1 = Database::open(DbConfig::new(dir.path().join("test.db")))?;
let mut db2 = db1.connect()?;

db1.execute("BEGIN")?;
db2.execute("BEGIN")?;
db1.execute("COMMIT")?;
db2.execute("COMMIT")?;
```

`cargo test -p engine --test sessions` runs this file alone. Every test
opens its own `tempfile::tempdir()`, so they can run concurrently with
each other and with every other integration test in this crate without
colliding on a database file; tests that assert a timeout keep the
configured deadline in the low hundreds of milliseconds, and every test
that spawns worker threads of its own bounds them with
`CONCURRENT_TEST_TIMEOUT`/`recv_within`, so the whole file still finishes
quickly and fails loudly rather than hanging if a regression reintroduces
a stall on the engine's dispatch thread.
