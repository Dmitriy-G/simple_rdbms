# snapshot_visibility

Proves `SeqScanExecutor`/`IndexScanExecutor` actually honor
`txn::IsolationLevel::SnapshotIsolation`: a scan running under it takes no
table or row locks and instead consults `ctx.version_store` to skip a row
its own snapshot cannot see, while a scan under any other isolation level
is unaffected - still locking exactly as `seq_scan.MD`/`index_scan.MD`
describe. Also proves the narrower guarantee `InsertExecutor` depends on
for that to be safe under real concurrency: that a row's heap bytes and
its `VersionStore` chain become observable together, never one before the
other (`executor::operators::insert.MD`'s "MVCC version stamping",
`storage::heap.MD`'s `TableHeap::insert_tuple_with`). Drives a real
`txn::TransactionManager` rather than hand-rolled `Transaction`s, since
the whole point is to exercise real `read_ts`/`commit_ts` allocation and
`VersionStore` wiring end to end, which the ephemeral, per-call
`LockManager`/`VersionStore` `tests/support/mod.rs`'s `run_to_completion`
builds cannot provide (this file does not declare `mod support;` for
that reason - it needs one `LockManager`/`VersionStore` shared across
every `ExecutorContext` it builds within a test, not a fresh pair per
call).

## Key Components

- `RACE_INSERT_COUNT` - `300`: how many rows the writer thread in
  `seq_scan_under_snapshot_isolation_never_observes_a_row_before_its_version_chain_exists`
  inserts, giving the reader thread that many chances to race the write.
- `RACE_TEST_TIMEOUT` - `30` seconds; the bound the same test's
  `finished_rx.recv_timeout` uses so a reintroduced race that hangs
  (rather than merely returns a wrong row) fails the test instead of the
  suite.
- `open_pool(pool_size) -> (BufferPool, TempDir)` - private, `#[cfg(test)]`.
  The same temp-dir-backed `BufferPool` construction `tests/seq_scan.rs`
  uses, copied rather than shared via `tests/support/mod.rs` for the
  reason above.
- `row(value) -> Vec<BoundExpr>` - private, `#[cfg(test)]`. Wraps a single
  `Value::Integer` in a `BoundExpr::Literal`, building an
  `InsertExecutor`-ready one-column row directly.
- `as_integer(tuple) -> i32` - unwraps a single-`Integer`-column tuple,
  panicking on any other shape.
- `insert_row(txn_manager, pool, catalog, table_id, value) -> TxnId` -
  begins a `ReadCommitted` writer, builds an `ExecutorContext` from
  `txn_manager`'s own shared `lock_manager`/`version_store`, runs
  `InsertExecutor` for one row, commits, and returns the writer's
  `TxnId`. The shared point: every call routes through the *same*
  `Arc<LockManager>`/`Arc<VersionStore>` `txn_manager` owns, so a later
  scan's visibility checks see every commit this helper made.
- `seq_scan_under_snapshot_isolation_hides_uncommitted_rows_but_shows_untracked_ones` -
  seeds one row directly via `TableHeap::insert_tuple` (bypassing
  `InsertExecutor`/`VersionStore` entirely, so it is a row the store has
  never heard of - as if it predates this process), begins a
  `SnapshotIsolation` reader, then commits a second row through
  `insert_row` *after* the reader's snapshot began. A `SeqScanExecutor`
  run with that reader's transaction returns only the untracked row: the
  "no chain for this `Rid`" row is visible per `txn::version_store.MD`,
  and the row committed after `read_ts` is not.
- `seq_scan_under_snapshot_isolation_sees_its_own_uncommitted_insert` - a
  single transaction inserts a row via `InsertExecutor` and, without
  committing, runs a `SeqScanExecutor` under its own `ExecutorContext`:
  the row it just inserted comes back, proving `VersionStore::is_visible_to`
  (`txn::version_store.MD`) is what `SeqScanExecutor` calls, not the
  plainer `is_visible` - a transaction that could not see its own
  uncommitted write would make `BEGIN; INSERT ...; SELECT ...` return
  nothing, which is a correctness bug the milestone's own acceptance test
  (`crates/engine/tests/explain.rs`'s
  `explain_insert_inside_open_transaction_leaves_the_transaction_untouched`)
  caught at the full-engine level before this narrower regression test
  was added here.
- `seq_scan_under_read_committed_still_takes_its_locks` - the same setup
  under plain `ReadCommitted` instead: after a full scan,
  `LockManager::held_lock_count` for the reader's `TxnId` is nonzero,
  proving the isolation-level branch in `SeqScanExecutor::init`/`next`
  only skips locking under `SnapshotIsolation`, not generally.
- `index_scan_under_snapshot_isolation_hides_the_same_invisible_row` -
  same shape as the first test but through `IndexScanExecutor` over a
  real B+tree index: a row committed before the reader begins is visible,
  one committed after is not.
- `insert_registers_a_version_chain_before_the_rid_is_returned` -
  deterministic, single-threaded check of the postcondition
  `storage::heap::TableHeap::insert_tuple_with`'s callback contract
  (`storage::heap.MD`) is meant to guarantee: begins a `SnapshotIsolation`
  observer *before* a separate writer runs `InsertExecutor`, then,
  immediately after `InsertExecutor::next` returns, looks up the inserted
  row's `Rid` via `TableHeap::iter` and asserts `VersionStore::is_visible_to`
  is already `false` for the observer (whose snapshot predates the
  insert) and already `true` for the writer itself. This alone cannot
  distinguish the fixed call order from the old, racy one - by the time
  `InsertExecutor::next` returns, both orders have already called
  `record_insert` - so it is a sanity check that a version chain always
  ends up recorded, not a reproduction of the race; the next test is what
  actually exercises the ordering.
- `seq_scan_under_snapshot_isolation_never_observes_a_row_before_its_version_chain_exists` -
  the concurrent regression test for the same invariant: a writer thread
  runs `InsertExecutor` for `RACE_INSERT_COUNT` rows back to back inside
  one transaction that is never committed, while a reader thread
  concurrently builds a fresh `SeqScanExecutor` in a tight loop under
  `IsolationLevel::SnapshotIsolation` and records the value of any row it
  ever sees over an `mpsc` channel. Both threads share the `pool` and
  `catalog` behind `Arc` (rather than `thread::scope`, since the shared
  `TransactionManager` state - `txn_manager.lock_manager()`/
  `version_store()` - already hands out `Arc` clones) so they can run
  truly concurrently instead of one waiting on the other. The main thread
  bounds the whole test with `RACE_TEST_TIMEOUT` via
  `finished_rx.recv_timeout`, then asserts the violations channel is
  empty: reverting `executor::InsertExecutor::next` to call
  `heap.insert_tuple` followed by a separate `record_insert` (the bug
  this regression test guards against) reproducibly fails this assertion
  with rows the reader should never have been able to see.

## Usage Example

```rust
let (pool, _dir) = open_pool(16);
let mut catalog = Catalog::new();
let mut txn_manager = TransactionManager::new(None);
// ... create table ...

let reader = txn_manager.begin(&pool, IsolationLevel::SnapshotIsolation)?;
let reader_txn = txn_manager.get(reader)?.clone();
insert_row(&mut txn_manager, &pool, &catalog, table_id, 1);

let lock_manager = txn_manager.lock_manager().clone();
let version_store = txn_manager.version_store().clone();
let mut ctx = ExecutorContext::new(&catalog, &pool, &reader_txn, &lock_manager, &version_store);
let mut scan = SeqScanExecutor::new(table_id);
scan.init(&mut ctx)?;
assert!(scan.next(&mut ctx)?.is_none());
```

Run as part of `cargo test --workspace`, or just this file with `cargo
test -p executor --test snapshot_visibility`; together with
`tests/seq_scan.rs`'s existing locking assertions (which this file leaves
untouched and still passing), this is the executor-level proof that
M10.3's snapshot isolation actually changes what a scan returns and does
not, rather than only the `txn` crate's own lower-level tests
(`txn/tests/lifecycle.MD`, `txn/tests/version_store.MD`) exercising the
mechanism in isolation.
