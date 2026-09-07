# catalog_reload_race

Regression coverage for P-46: `EngineShared::reload_catalog`
(`crates/engine/src/runtime.rs`, `runtime.MD`) used to build the
replacement `Catalog` with `Catalog::open` before taking `catalog`'s write
lock, so a reload triggered by one session's aborted/rolled-back
autocommit statement could install a catalog that missed a concurrent
session's just-committed `CREATE TABLE`, or that reverted an index's root
page id to a pre-split value a concurrent `InsertExecutor` had already
moved past. The fix holds `catalog`'s write lock across the whole reload
- `Catalog::open` included, not just the final swap - so a reload now
excludes both the `catalog` *write* lock `CreateTable`/`CreateIndex` hold
for their own mutation and the `catalog` *read* lock `run` holds for an
`Insert`/`Select`'s whole `init`/`next` loop. Both tests here synchronize
their two sessions on a fresh `std::sync::Barrier` every round, the same
pattern `crates/storage/tests/buffer_pool_concurrency.rs` uses, rather
than on a sleep - a barrier makes the two sessions' critical sections
actually overlap instead of merely hoping they do.

## Key Components

- `CREATE_ROUNDS`/`SPLIT_ROUNDS`/`SPLIT_BATCH` - round counts and batch
  size shared by the two tests below. `SPLIT_ROUNDS` (300) one-row rounds
  - rather than fewer, larger batches - is what makes the second test's
  race reproducible: each round gives the two sessions exactly one narrow
  window where a root split's `update_index_root_page` write and a
  concurrent `reload_catalog`'s read of the same catalog row can
  interleave, and 300 independent tries at that window catches it
  reliably where 10 batched rounds (tried first, against the pre-fix
  code) did not fail even once in eight runs - confirmed against the
  pre-fix code (`git stash` the `runtime.rs` half of this change) both
  ways: the tuned parameters fail every one of five runs before the fix
  and pass every one of five after it.
- `a_committed_table_survives_a_concurrent_catalog_reload` - one session
  runs `CREATE TABLE` while a second, on the same barrier, sends a
  statement against a table that doesn't exist - guaranteed to fail
  binding and take `execute_non_control_statement`'s autocommit-failure
  path (`abort_txn` then `reload_catalog`, `runtime.rs`) - for
  `CREATE_ROUNDS` rounds, one freshly named table (`t0`, `t1`, ...) per
  round. Afterward every table name is asserted present through
  `Database::table_names`. Before the fix this reliably failed: a reload
  racing a `create_table` whose `catalog_heap.insert_tuple` had already
  landed but whose write-lock scope reload didn't yet exclude would
  install a catalog with that table missing.
- `an_index_root_split_survives_a_concurrent_catalog_reload` - creates
  `t (a BIGINT)` and an index on `a`, then for `SPLIT_ROUNDS` rounds
  inserts one new key on one barrier-synchronized session while the other
  session's failing statement drives another `reload_catalog`; by
  `SPLIT_ROUNDS * SPLIT_BATCH` (300) keys the `BTreeIndex` has split its
  root at least once, so `InsertExecutor::next`'s `update_index_root_page`
  call (`crates/executor/src/operators/insert.MD`) fires during the round
  it happens. Afterward, a single ranged `SELECT ... WHERE a >= 0 AND a <
  total` - which `planner::optimizer::IndexScanRule` turns into an
  `IndexScan` - must return exactly the full `0..total` set. Before the
  fix, a reload that read an index's catalog row before a concurrent root
  split's `update_index_root_page` write and installed itself after could
  revert the in-memory root page id to the pre-split page, silently
  stranding later inserts in a subtree the real root's separators don't
  cover.

## Usage Example

```rust
let dir = tempfile::tempdir()?;
let mut db0 = Database::open(DbConfig::new(dir.path().join("test.db")))?;
let mut creator = db0.connect()?;
let mut roller = db0.connect()?;

let barrier = Barrier::new(2);
thread::scope(|scope| {
    scope.spawn(|| { barrier.wait(); creator.execute("CREATE TABLE t (a INTEGER)") });
    scope.spawn(|| { barrier.wait(); let _ = roller.execute("SELECT * FROM missing"); });
});
```

`cargo test -p engine --test catalog_reload_race` runs this file alone.
Each test opens its own `tempfile::tempdir()`, so the two can run
concurrently with each other and with every other integration test in
this crate.
