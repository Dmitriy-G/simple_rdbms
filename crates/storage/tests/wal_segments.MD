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
- `open_counting(path, target_segment_size) -> (LogManager, calls, bytes)` -
  opens a `LogManager` through a fresh `support::CountingSegmentStore`
  (`support/mod.MD`), returning the shared call/byte counters alongside it
  - the mechanism `opening_a_long_log_reads_only_the_live_tail` uses to
  prove `open` reads a bounded number of bytes rather than one
  proportional to total history. Passes a throwaway, never-read `opens`
  counter, since nothing in this file needs it -
  `crates/storage/tests/undo_performance.rs` is the one that does.
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
- `a_roll_survives_an_unclean_reopen` - appends 100 filler records (forcing
  at least one roll at `SMALL_SEGMENT`), then reopens with no clean
  shutdown in between and asserts both `segment_ids()` and every record's
  LSN (via `iter_from(Lsn(HEADER_LEN))`) are unchanged. A misread or
  never-written header on the newly-rolled active segment would surface
  here either as a hard error, a shifted LSN, or lost records - the
  regression `write_segment_header`'s `sync_all()` exists to prevent.
- `a_headerless_active_segment_continues_the_sealed_lsn_space` - appends
  400 filler records, then truncates the active (highest-numbered) segment
  file to zero bytes directly on disk, simulating a crash between
  `roll_segment` making a segment active and its header ever reaching
  disk. Before truncating, reads that segment's own `start_lsn` straight
  out of its still-intact header bytes (offset 8..16) as
  `original_active_start_lsn` - the value `bootstrap` must resume from.
  Reopens (triggering `bootstrap`'s headerless-active-segment branch) and
  appends one more record, asserting its LSN is `>=`
  `original_active_start_lsn`. Deliberately does not compare against the
  highest LSN appended before the truncation: those records lived only in
  the now-destroyed active segment and are expected to be gone (they were
  never durable), unlike anything a *sealed* segment still holds, which
  the round-2 bug this test targets would have silently overwritten.
- `a_corrupt_sealed_segment_is_refused_rather_than_silently_shortened` -
  appends 400 filler records, then flips one byte in the middle of the
  *first* sealed segment's file directly on disk (its CRC framing makes
  that single flip enough to fail decoding at exactly that record, well
  before the segment's real end). Reopening must return
  `StorageError::CorruptLogHeader`, not silently succeed - but the
  assertion that actually matters, and the one a naive "reopen errors"
  test would miss, is the one that comes after: the segment file's length
  on disk must be byte-for-byte unchanged. Before `scan_sealed_segment`
  existed, `bootstrap` ran the same trimming scan against sealed segments
  that the active segment legitimately needs, so this exact scenario would
  have both errored *and* already truncated the corrupted segment out from
  under the caller - a silent, unrecoverable loss of everything after the
  flipped byte, on a segment nothing should ever be allowed to shorten.

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
