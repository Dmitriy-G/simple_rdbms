# snapshot_visibility

Proves `SeqScanExecutor`/`IndexScanExecutor` actually honor
`txn::IsolationLevel::SnapshotIsolation`: a scan running under it takes no
table or row locks and instead consults `ctx.version_store` to skip a row
its own snapshot cannot see, while a scan under any other isolation level
is unaffected - still locking exactly as `seq_scan.MD`/`index_scan.MD`
describe. Drives a real `txn::TransactionManager` rather than
hand-rolled `Transaction`s, since the whole point is to exercise real
`read_ts`/`commit_ts` allocation and `VersionStore` wiring end to end,
which the ephemeral, per-call `LockManager`/`VersionStore`
`tests/support/mod.rs`'s `run_to_completion` builds cannot provide (this
file does not declare `mod support;` for that reason - it needs one
`LockManager`/`VersionStore` shared across every `ExecutorContext` it
builds within a test, not a fresh pair per call).

## Key Components

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
- `seq_scan_under_read_committed_still_takes_its_locks` - the same setup
  under plain `ReadCommitted` instead: after a full scan,
  `LockManager::held_lock_count` for the reader's `TxnId` is nonzero,
  proving the isolation-level branch in `SeqScanExecutor::init`/`next`
  only skips locking under `SnapshotIsolation`, not generally.
- `index_scan_under_snapshot_isolation_hides_the_same_invisible_row` -
  same shape as the first test but through `IndexScanExecutor` over a
  real B+tree index: a row committed before the reader begins is visible,
  one committed after is not.

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
