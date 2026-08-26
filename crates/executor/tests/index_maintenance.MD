# index_maintenance

`InsertExecutor`'s index-upkeep behavior: a single index and every index
on a multi-indexed table both get maintained as rows are inserted, and an
unorderable indexed value (NaN) fails the statement rather than being
silently indexed.

## Key Components

- `insert_maintains_a_single_index` - creates a table with one index,
  inserts three distinct values via `InsertExecutor`, then opens the
  index directly (`storage::btree::BTreeIndex::open` at the root page
  `Catalog::index_root_page` reports) and confirms `get` finds each value
  exactly once.
- `insert_maintains_every_index_on_a_multi_indexed_table` - a two-column
  table with an index on each column; one inserted row must show up in
  both indexes.
- `a_failed_index_insert_returns_an_error_and_leaves_that_row_out_of_the_index` -
  a `Double`-indexed table with one normal row and one `f64::NAN` row.
  Drives `InsertExecutor::init`/`next` by hand (rather than
  `support::run_to_completion`, which panics on `Err`) to observe `next`
  return `Err`. Confirms both heap rows exist (the heap insert for the
  failing row completes before the index insert that fails on it - see
  `insert.MD`) and the index holds only the successful row's entry -
  pinning down the mechanics `insert.MD` describes, not the full
  transactional rollback guarantee, which is
  `engine`-level (`crates/engine/tests/`): this crate has no abort
  machinery of its own to exercise.

## Usage Example

```rust
mod support;

let (pool, _dir) = support::open_pool(16);
let mut catalog = Catalog::new();
let table_id = catalog.create_table(&pool, TXN, "t", schema).expect("create table").table_id;
let index_id = catalog.create_index(&pool, TXN, "idx", table_id, 0).expect("create index").index_id;

let mut insert = InsertExecutor::new(table_id, vec![support::row(vec![Value::Integer(1)])]);
let txn = Transaction::new(TXN, IsolationLevel::ReadCommitted);
support::run_to_completion(&pool, &catalog, &txn, &mut insert);
```

Run as part of `cargo test --workspace`, or just this file with
`cargo test -p executor --test index_maintenance`.
