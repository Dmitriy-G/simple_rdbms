# seq_scan

`SeqScanExecutor`'s lazy, page-at-a-time cursor: tuples come back in
storage order without materializing the whole table, laziness is
observable (pulling one tuple never fetches pages beyond the first), and
two scans interleaved over the same table each still yield the full,
correct result - exercising the buffer pool's sole-owner pin protocol
alongside the cursor.

## Key Components

- `open_pool(pool_size) -> (BufferPool, TempDir)` - builds a
  `BufferPool` backed by a temp-dir disk, double-write buffer, WAL, and
  `LruKReplacer` of the given size. Marked `#[cfg(test)]`, as is
  `seed_table` below, so `clippy.toml`'s `allow-expect-in-tests` applies
  to their `.expect(...)` calls - see `storage/tests/recovery_logging.MD`
  for why a free function in a `tests/` file needs the attribute spelled
  out explicitly.
- `seed_table(pool, catalog, count) -> (TableId, Vec<i64>)` - creates a
  single-column (`n BIGINT`) table and inserts `n` for `n` in `0..count`,
  enough rows to span several pages against a small pool. Returns the
  table's id and the values expected back, in scan order.
- `as_bigint(tuple) -> i64` - unwraps a single-`BigInt`-column tuple,
  panicking on any other shape.
- `scan_spanning_pages_preserves_insertion_order` - seeds 800 rows into a
  2-frame pool and asserts a full scan returns them in insertion order.
- `pulling_one_tuple_never_fetches_pages_beyond_the_first` - seeds 800
  rows into a 4-frame pool, resets the fetch counter after `init`, pulls
  one tuple, and asserts exactly one page fetch occurred.
- `interleaved_scans_over_the_same_table_each_yield_the_full_result` -
  runs two `SeqScanExecutor`s over the same table in lockstep (alternating
  `next` calls) and asserts each independently produces the full expected
  result.

## Usage Example

```rust
let (pool, _dir) = open_pool(2);
let mut catalog = Catalog::new();
let (table_id, expected) = seed_table(&pool, &mut catalog, 800);

let txn = Transaction::new(TxnId(0), IsolationLevel::ReadCommitted);
let mut ctx = ExecutorContext::new(&catalog, &pool, &txn);
let mut scan = SeqScanExecutor::new(table_id);
scan.init(&mut ctx).expect("init");
```

Run as part of `cargo test --workspace`; these are integration tests
(under `tests/`, not `src/`) so they exercise `SeqScanExecutor` through
its public API only, against a real `BufferPool` and `Catalog` rather
than mocks.
