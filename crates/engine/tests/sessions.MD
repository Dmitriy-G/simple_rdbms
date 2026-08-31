# sessions

Integration coverage for M14.1's admission control (`docs/ROADMAP.md`'s
M14 entry, `crates/engine/src/runtime.MD`): the rule
that at most one session may hold an explicit transaction at a time, that
a second session's `BEGIN` (or autocommit statement) waits and then fails
rather than running concurrently, that a disconnect or an idle timeout
releases the slot rather than abandoning it, and that the engine-wide
bookkeeping (checkpointing, a dead engine thread) behaves the same
whether one session or several are driving it. Every test here drives
more than one session against one database through `Database::connect`
(`crates/engine/src/database.MD`), which nothing under `crates/engine/tests/`
needed before this file - `logging.rs` and everything else exercises
exactly one connection.

## Key Components

- `row_count(ResultSet) -> usize` - private helper. Unwraps a `SELECT`'s
  row count, panicking with the actual variant's name for any other
  `ResultSet` shape, so an assertion failure names what it got instead of
  panicking inside a bare `match` with no message.
- `short_lock_wait(dir) -> DbConfig`/`long_lock_wait(dir) -> DbConfig` -
  private helpers building a `DbConfig` at `dir/test.db` with
  `lock_wait_timeout_ms` set to 100ms or 5,000ms respectively - short for
  tests that want a parked request to actually time out without a slow
  test run, long for tests that want a parked request to sit safely past
  the time the holder takes to free the slot on its own.
- `a_second_session_cannot_open_a_concurrent_transaction` - `db1` holds an
  explicit transaction (`short_lock_wait`); `db2.execute("BEGIN")` from
  the same call returns `Error::LockTimeout` (`55P03`) rather than
  succeeding, once the deadline passes with nobody granting it - the
  central guarantee `runtime.MD`'s admission-control section describes.
- `a_parked_begin_is_granted_when_the_holder_commits` - `db1` holds a
  transaction (`long_lock_wait`, so the deadline can't fire first); a
  second thread's `db2.execute("BEGIN")` blocks; `db1.execute("COMMIT")`
  after a short sleep frees the slot, and the waiting thread's `BEGIN`
  succeeds - `try_admit_waiters` running from `handle_commit`, not only
  from `tick`.
- `a_parked_begin_fails_with_55p03_after_its_deadline` - same shape as the
  first test but asserts the elapsed time stays well under the deadline
  it would take to hang, guarding against a regression that turns a
  timeout into an indefinite block rather than only checking the returned
  error.
- `an_autocommit_statement_waits_for_an_open_transaction_to_finish` - the
  autocommit half of admission control: `db2.execute("INSERT ...")` with
  no `BEGIN` of its own still parks behind `db1`'s open transaction and is
  granted once `db1` commits, and the inserted row is visible afterward -
  proving `peek_needs_txn_slot` treats a bare autocommit statement as
  needing the slot exactly like `BEGIN` does.
- `dropping_a_session_mid_transaction_rolls_it_back` - `db2` begins,
  inserts, and is dropped without `COMMIT`/`ROLLBACK`; the row is not
  visible afterward through `db1` - `disconnect`'s abort path, the fix for
  the failure mode where a disconnect used to leave the transaction
  active forever.
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
- `a_dead_engine_thread_fails_every_session_with_a_fatal_error` -
  `Database::kill_engine_for_test` (`#[cfg(any(test, feature =
  "test-util"))]`, `database.MD`) stops the engine thread without a real
  panic; both `db1` and the independently-connected `db2` report
  `Error::EngineUnavailable` on their next statement rather than hanging,
  proving every session's channel fails the same way once the thread is
  gone, regardless of which session triggered the shutdown.
- `checkpoint_count(threshold, drive, statements) -> Result<usize>` -
  private helper. Installs a capturing subscriber
  (`tests/support/mod.MD`) before `Database::open`, creates a table, runs
  `drive` against the open `Database`, and counts how many "checkpoint
  complete" events (`txn::checkpoint::write_checkpoint`) were logged - the
  observable proxy for how many times `EngineState::maybe_checkpoint`
  actually decided to checkpoint.
- `several_sessions_checkpoint_once_per_threshold_not_once_each` - runs
  the same forty `INSERT`s twice with the same `checkpoint_byte_threshold`:
  once through one session, once split alternately across two sessions
  connected to the same `Database`. Asserts the single-session run
  produces at least one checkpoint (the growth was large enough to prove
  something), and that the split run produces the *same* count, not
  double - regression coverage for the pre-M14.1 bug where each
  `Database` tracked `bytes_at_last_checkpoint` itself and N connections
  against one log checkpointed N times as often; `EngineState` now being
  the one place that field lives is what keeps this test passing.

## Usage Example

```rust
use common::DbConfig;
use engine::Database;

let dir = tempfile::tempdir()?;
let mut db1 = Database::open(DbConfig::new(dir.path().join("test.db")))?;
let mut db2 = db1.connect()?;

db1.execute("BEGIN")?;
let err = db2.execute("BEGIN").unwrap_err();
assert_eq!(err.sql_state(), common::SqlState::LOCK_NOT_AVAILABLE);
```

`cargo test -p engine --test sessions` runs this file alone. Every test
opens its own `tempfile::tempdir()`, so they can run concurrently with
each other and with every other integration test in this crate without
colliding on a database file; tests that assert a timeout keep the
configured deadline in the low hundreds of milliseconds so the whole file
still finishes quickly even though several tests deliberately wait one
out.
