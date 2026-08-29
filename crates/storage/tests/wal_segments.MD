# wal_segments

`LogManager`'s segmented write-ahead log behavior
(`docs/adr/0011-segmented-write-ahead-log.md`): reopening a long log stays
bounded, `truncate_below` actually deletes what it should and only what it
should, an active transaction's own segment survives truncation even when
its most recent activity is elsewhere, and recovery still reproduces the
correct committed state when the workload it replays spans more than one
segment.

Every test uses `LogManager::open_with_segment_size`/`open_with_segment_store`
(`test-util`-only) with a small `SMALL_SEGMENT` (512 bytes) instead of
`DEFAULT_SEGMENT_SIZE`'s 16 MiB, so a few hundred small records are enough
to force real rollover without the test taking a meaningful amount of time
or disk.

## Key Components

- `SMALL_SEGMENT` - the target segment size every test in this file uses,
  small enough that a few hundred filler records reliably span three or
  more segments.
- `CountingSegmentStore` - a `SegmentStore` wrapping a real
  `FileSegmentStore`, whose every opened segment device is wrapped in
  `support::CountingDevice` sharing one pair of call/byte counters across
  every segment the log opens over its lifetime - the mechanism
  `opening_a_long_log_reads_only_the_live_tail` uses to prove `open` reads
  a bounded number of bytes rather than one proportional to total history.
- `open_counting(path, target_segment_size) -> (LogManager, calls, bytes)` -
  opens a `LogManager` through a fresh `CountingSegmentStore`, returning
  the shared counters alongside it.
- `open_pool(dir, target_segment_size) -> BufferPool` - like the `open_pool`
  helper in `recovery.rs`, but plumbing a caller-chosen segment size
  through to the `LogManager` instead of the default.
- `filler(log, txn_id) -> Result<Lsn, StorageError>` - appends and flushes
  one throwaway `Update` record for `txn_id`, returning its LSN. The
  all-purpose way every test here manufactures enough log volume to force
  segment rollover without needing a real page or buffer pool.
- `opening_a_long_log_reads_only_the_live_tail` - appends 400 filler
  records (spanning several segments at `SMALL_SEGMENT`), truncates
  everything below the last LSN (simulating a checkpoint with nothing left
  to protect), closes, and reopens through a fresh counting store.
  Asserts the reopen reads fewer than two segments' worth of bytes -
  proportional to one segment, not to the 400 records' total history.
- `a_checkpoint_deletes_segments_below_the_recovery_bound` - appends 400
  filler records, confirms at least three segments exist, then
  `truncate_below`s a bound in the middle of that history. Asserts the
  segment count actually dropped, a record from before the bound is no
  longer readable (`read_at` returns `None` - its segment is gone), and a
  record from after the bound still is.
- `a_transaction_open_across_a_checkpoint_can_still_be_undone` - logs a
  `Begin` for one transaction, then 400 unrelated filler records (forcing
  several rollovers), then one more record for the *same* transaction, far
  from its `Begin` in LSN space. Truncates below the transaction's
  `begin_lsn` - exactly the bound `txn::write_checkpoint` computes via
  `TransactionManager::earliest_active_begin_lsn`, not the transaction's
  most recent LSN - and asserts both the `Begin` and the later record are
  still readable, with the later record's `prev_lsn` still correctly
  chained back to it. This is the concrete case
  `docs/adr/0011-segmented-write-ahead-log.md` explains: bounding by a
  transaction's most recent LSN instead would have let this exact
  scenario truncate a segment its own undo chain still needs.
- `recovery_across_a_segment_boundary_reproduces_the_committed_prefix` -
  writes to one page, then drives 200 unrelated writes to force the log
  across several segment boundaries, then overwrites the same page again
  and commits, all without a clean shutdown. Reopens with
  `storage::recovery::recover` and asserts the page holds the final,
  committed value - proving redo still finds and correctly orders records
  that live in different segment files, not just within one.

## Usage Example

```rust
let (log, calls, bytes) = open_counting(&path, SMALL_SEGMENT)?;
for i in 0..400u64 {
    filler(&log, TxnId(i % 5))?;
}
log.truncate_below(log.durable_lsn())?;
```

Run via `cargo test -p storage --test wal_segments`. Complements
`crates/storage/tests/wal.rs` (single-segment record framing and CRC/torn-write
handling) and `crates/storage/tests/recovery.rs` (single-segment Analysis/Redo/
Undo) by exercising the same guarantees once more than one segment file is
in play.
