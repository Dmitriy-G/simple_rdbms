# snapshot_isolation

End-to-end proof that M10.3's snapshot isolation actually changes what a
`SELECT` sees across two real `Database` sessions - `runtime.MD`'s M10.3
note describes the mechanism (`txn::VersionStore`, every transaction
begun under `txn::IsolationLevel::SnapshotIsolation`); this file drives it
through the public `Database::execute` API the way `transactions.rs` and
`sessions.rs` drive the rest of transaction handling, rather than
`executor`'s own lower-level `tests/snapshot_visibility.rs`, which
exercises `SeqScanExecutor`/`IndexScanExecutor` directly against a hand-
built `ExecutorContext`.

## Key Components

- `CONCURRENT_TEST_TIMEOUT` - the same 10-second bound
  `tests/transactions.rs` uses for anything that could, on a regression,
  hang instead of failing.
- `recv_within(rx, timeout, what) -> T` - same helper as
  `tests/transactions.rs`'s: panics with a clear message instead of
  blocking forever if `timeout` elapses.
- `a_concurrent_select_does_not_block_behind_an_uncommitted_insert` -
  session A `BEGIN`s and inserts a row without committing; session B's
  `SELECT`, run on a separate thread and awaited with a bounded
  `recv_timeout`, returns promptly and sees no rows. Before M10.3 this
  would have blocked until A's commit released the shared table lock a
  `SeqScanExecutor` used to take in `init`; under `SnapshotIsolation` it
  takes no such lock at all (`executor::operators::seq_scan.MD`).
- `a_readers_snapshot_does_not_move_after_another_sessions_commit` -
  session B begins an explicit transaction before session A inserts and
  commits a row; a `SELECT` inside B's still-open transaction does not see
  it, proving B's snapshot was fixed at its own `BEGIN` and does not
  advance just because time (and another session's commit) passed. A
  third session begun *after* A's commit does see the row.
- `a_rolled_back_insert_is_never_visible_to_a_later_transaction` - session
  A inserts a row and rolls back; neither a later statement on the same
  session nor a fresh session ever sees it. `TransactionManager::abort`'s
  WAL undo removes the row from the heap page itself, so this does not
  depend on `VersionStore::is_visible_to`'s "no chain for this `Rid`" default
  ever being asked about a `Rid` with no physical tuple behind it
  (`txn::version_store.MD`'s open question on `abort_versions` - moot here
  because the row is gone from the heap entirely, not merely absent from
  the version store).

## Usage Example

```rust
let dir = tempfile::tempdir()?;
let mut db1 = Database::open(DbConfig::new(dir.path().join("test.db")))?;
db1.execute("CREATE TABLE t (a INTEGER)")?;
let mut db2 = db1.connect()?;

db1.execute("BEGIN")?;
db1.execute("INSERT INTO t VALUES (1)")?;
assert_eq!(rows_of(db2.execute("SELECT * FROM t")?), Vec::<Vec<Value>>::new());
db1.execute("COMMIT")?;
```

Run as part of `cargo test --workspace`, or just this file with `cargo
test -p engine --test snapshot_isolation`; together, these three tests are
the milestone's acceptance test - the roadmap's "a reader no longer
blocks behind a writer holding locks on the same rows" claim, checked
against the actual public `Database` surface rather than only against
`executor`'s or `txn`'s own internals.
