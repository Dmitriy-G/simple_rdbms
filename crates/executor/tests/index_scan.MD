# index_scan

`IndexScanExecutor`'s range-scan behavior against a real
`storage::btree::BTreeIndex`: ascending order, bounded ranges, duplicate
keys, and correctness across a root split.

## Key Components

- `seed(pool, count) -> (Catalog, TableId, IndexId)` - creates a
  single-column (`id INTEGER`) table with an index on it and inserts `i`
  for `i` in `0..count` via one `InsertExecutor` call. Marked
  `#[cfg(test)]` so `clippy.toml`'s `allow-expect-in-tests` applies to its
  `.expect(...)` calls (`tests/seq_scan.MD` explains why a free function
  in a `tests/` file needs the attribute spelled out).
- `scan_values(pool, catalog, index_id, table_id, start, end) -> Vec<i32>` -
  runs an `IndexScanExecutor` over `[start, end)` to completion via
  `support::run_to_completion` and unwraps each returned tuple's single
  `Integer` column. Also `#[cfg(test)]`, same reason as `seed`.
- `index_scan_finds_every_row_in_ascending_key_order` - an unbounded scan
  over 50 rows returns exactly `0..50`, in order.
- `index_scan_respects_start_and_end_bounds` - a scan over
  `[encoded(5), encoded(10))` against 20 seeded rows returns exactly
  `5..10`.
- `index_scan_returns_every_duplicate_key` - five rows sharing one
  indexed value all come back from an unbounded scan.
- `index_scan_after_a_root_split_still_finds_every_row` - seeds 3,000 rows
  into a 64-frame pool (enough to force a B+tree root split, the same
  shape `crates/storage/tests/btree.rs`'s split tests use) and asserts an
  unbounded scan still finds every one of them. This is the test that
  would fail if `executor::InsertExecutor` forgot to call
  `catalog::Catalog::update_index_root_page` after a split - `IndexScanExecutor`
  would then open the index at a stale root and silently miss whatever
  moved out of that subtree.

## Usage Example

```rust
mod support;

let (pool, _dir) = support::open_pool(16);
let (catalog, table_id, index_id) = seed(&pool, 50);
let values = scan_values(&pool, &catalog, index_id, table_id, None, None);
assert_eq!(values, (0..50).collect::<Vec<_>>());
```

Run as part of `cargo test --workspace`, or just this file with
`cargo test -p executor --test index_scan`.
