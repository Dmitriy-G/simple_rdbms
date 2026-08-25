# undo_performance

`storage::recovery::undo_transaction`'s I/O cost under a long undo chain,
exercised entirely through public API (`undo_transaction`, `BufferPool`,
`DiskManager`, `LogManager`, `DoubleWriteBuffer`, `LruKReplacer`) plus the
`CountingDevice` helper from `tests/support/mod.rs`. Formerly an inline
`#[cfg(test)] mod tests` in `crates/storage/src/recovery.rs`; moved here
once `CountingDevice` itself moved out from behind `#[cfg(test)]`, since
nothing in this test touches a private item.

## Key Components

- `undoing_thousands_of_updates_reads_the_log_a_bounded_number_of_times` -
  rolling back thousands of updates must cost a small, constant amount of
  device I/O *per undone record*, not a whole extra log-sized read for
  every single one of them - the quadratic behavior `LogManager::read_at`
  replaced `iter_from(lsn).next()` to fix. Wraps the WAL's device in a
  `CountingDevice`, undoes 5,000 updates to one page, and asserts both the
  call count and cumulative bytes read stay within a small constant factor
  of the update count - a read counter is the deterministic way to check
  this; a wall-clock timing assertion would be flaky.

## Usage Example

```rust
let device: Box<dyn BlockDevice> =
    Box::new(CountingDevice::new(Box::new(FileDevice::new(wal_file)), calls.clone(), bytes.clone()));
let log = LogManager::open_with_device(device)?;
let pool = BufferPool::new(disk, dwb, log, 8, Box::new(LruKReplacer::new(8, 2)));
// ... 5,000 writes to one page, then reset the counters ...
undo_transaction(&pool, txn_id, last_lsn)?;
assert!(calls.get() <= 3 * N);
```

Run via `cargo test -p storage --test undo_performance`, alongside
`tests/recovery.rs`'s correctness-focused Analysis/Redo/Undo scenarios -
that file checks *what* recovery leaves behind, this one checks *how much
I/O* undoing a long chain costs to produce it.
