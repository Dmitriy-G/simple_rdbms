# undo_performance

`storage::recovery::undo_transaction`'s I/O cost under a long undo chain,
exercised entirely through public API (`undo_transaction`, `BufferPool`,
`DiskManager`, `LogManager`, `DoubleWriteBuffer`, `LruKReplacer`) plus the
`CountingDevice`/`CountingSegmentStore` helpers from the shared
`test-support` crate (`crates/test-support/src/devices.MD`) - see that
crate's docs for what each counter counts and why `opens` exists
separately from `calls`/`bytes`. Formerly an inline `#[cfg(test)] mod
tests` in `crates/storage/src/recovery.rs`; moved here once `CountingDevice`
itself moved out from behind `#[cfg(test)]`, since nothing in this test
touches a private item, and later moved again (along with `wal_segments.rs`'s
identical copy) into `test-support` so `catalog`, `executor`, `txn`, and
`engine`'s own `tests/` could reach the same wrapper too.

## Key Components

- `undoing_thousands_of_updates_reads_the_log_a_bounded_number_of_times` -
  rolling back thousands of updates must cost a small, constant amount of
  device I/O *per undone record*, not a whole extra log-sized read for
  every single one of them - the quadratic behavior `LogManager::read_at`
  replaced `iter_from(lsn).next()` to fix. Opens the WAL through a
  `CountingSegmentStore`, undoes 5,000 updates to one page, and asserts
  both the call count and cumulative bytes read stay within a small
  constant factor of the update count - a read counter is the
  deterministic way to check this; a wall-clock timing assertion would be
  flaky. Runs at `DEFAULT_SEGMENT_SIZE`, so every record stays in the
  active segment and `LogBufferInner::sealed_device_cache` (`wal.MD`)
  never comes into play - see the next test for that path.
- `undoing_across_sealed_segments_reads_the_log_a_bounded_number_of_times` -
  the same property, but for a chain that actually crosses sealed
  segments, which the test above cannot see. Opens the WAL through a
  `CountingSegmentStore` at `SMALL_SEGMENT` (512 bytes) and flushes after
  every one of 2,000 writes so the active segment actually rolls
  repeatedly; asserts at least four segments sealed before undoing (the
  precondition the test depends on) and, after undoing, that
  `SegmentStore::open` was called at most `sealed_segment_count + 2`
  times - not once per undone record. Without
  `LogBufferInner::sealed_device_cache`, `LogManager::read_at` reopens the
  relevant sealed segment on every single call as `undo_transaction` walks
  backward, so this assertion catches the regression the way the
  bytes/calls counters above cannot: that test's workload never leaves the
  active segment, so a per-record device open in the sealed-segment branch
  of `read_at` is invisible to it.

## Usage Example

```rust
let store: Arc<dyn SegmentStore> = Arc::new(CountingSegmentStore::new(
    FileSegmentStore::new(wal_path),
    calls.clone(),
    bytes.clone(),
    opens,
));
let log = LogManager::open_with_segment_store(store, DEFAULT_SEGMENT_SIZE)?;
let pool = BufferPool::new(disk, dwb, log, 8, Box::new(LruKReplacer::new(8, 2)));
// ... 5,000 writes to one page, then reset the counters ...
undo_transaction(&pool, txn_id, last_lsn)?;
assert!(calls.get() <= 3 * N);
```

Run via `cargo test -p storage --test undo_performance`, alongside
`tests/recovery.rs`'s correctness-focused Analysis/Redo/Undo scenarios -
that file checks *what* recovery leaves behind, this one checks *how much
I/O* undoing a long chain costs to produce it.
