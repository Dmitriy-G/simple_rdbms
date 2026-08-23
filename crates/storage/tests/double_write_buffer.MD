# double_write_buffer

The double-write buffer, mechanism-level: a batch round-trips through
`write_batch`/`read_batch`/`clear_batch`, a corrupted slot copy is
skipped rather than trusted, and — the targeted M12 deliverable — a real
page torn by a crash mid-flush is restored by
`recovery::recover_double_write` from its double-write copy, including at
the two sector boundaries and when the *restore* write itself is torn by
a second crash. See `docs/adr/0005-double-write-buffer-durability-boundary.md`
for the durability claim these tests check.

## Key Components

- `write_batch_then_read_batch_round_trips_page_ids_in_order` - writes two
  pages as a batch and checks `read_batch` returns their ids in order and
  `read_slot` returns each one's bytes.
- `clear_batch_makes_read_batch_report_nothing_to_recover` - after
  `clear_batch`, `read_batch` must report `None`.
- `recover_double_write_is_a_no_op_when_nothing_was_in_flight` - a
  freshly opened, never-used double-write buffer has no batch in flight:
  `recover_double_write` must be a clean no-op, leaving the real data
  file untouched.
- `a_corrupted_double_write_copy_is_skipped_not_restored_from` - a
  double-write copy corrupted after being written (its own checksum no
  longer verifies) must be treated as "the crash landed while this copy
  was still being written" - skipped, never trusted - leaving the real
  page exactly as it was, not overwritten with garbage.
- `recover_double_write_restores_a_page_torn_mid_flush` - the targeted
  deliverable: tears the real-file write for one specific dirtied page
  mid-`flush_all` (via a `FaultyDevice` wrapping only the data file, so
  the double-write buffer's own writes always land cleanly), tearing the
  first half of the page's sectors explicitly rather than a
  seeded-random subset, so this specific test's torn shape is guaranteed
  rather than incidental to what a given seed happens to land - the
  seeded-random behavior itself is exercised broadly by the
  crash-injection sweep in `crates/engine/tests/crash_injection.rs` and
  pinned down at its two boundaries by the sibling tests below. Asserts
  the real page fails its own checksum *before* recovery runs (otherwise
  the rest of the test would pass even if `recover_double_write` never
  ran at all), then proves `recovery::recover_double_write` - not just
  "recovery in general" - is what restores it.
- `assert_restores_a_page_torn_at_sectors(sectors)` - same shape as
  `recover_double_write_restores_a_page_torn_mid_flush`, but tears only
  the explicit 512-byte sector indices in `sectors`. Writes one region
  into sector 0 (bytes `20..30`) and another into sector 7, the last one
  (bytes `3600..3611`), so landing either sector alone still leaves the
  *other* region un-landed, and therefore a checksum mismatch, rather
  than risking an all-zero, sentinel-valid page if the landed sector
  happens to touch neither written region.
- `recover_double_write_restores_a_page_torn_at_only_its_first_sector` -
  the first boundary case: only sector 0 (bytes `0..512`, which includes
  the checksum and `page_lsn`) lands. The stamped-but-unmatched checksum
  alone is already enough for `checksum_ok` to reject the page.
- `recover_double_write_restores_a_page_torn_at_only_its_last_sector` -
  the interesting boundary case: only the *last* sector lands, leaving
  the old checksum in bytes `0..4` over a body that is otherwise old
  except for that one new tail sector - the mirror image of a
  first-sector-only tear. `checksum_ok` must still reject it: the stored
  (old) checksum was computed over the old body, which the new tail
  sector no longer matches.
- `recover_double_write_leaves_the_batch_in_place_when_its_own_restore_write_tears` -
  verify-before-clear (M12 part 3): phase 1 produces a torn real page
  with an intact double-write batch (tearing at an explicit sector rather
  than a seeded-random subset, so the mismatch this test depends on is
  guaranteed). Phase 2 attempts recovery, but tears the very first write
  issued *during recovery itself* - the restore write for the page a
  prior crash left torn - so recovery dies again mid-repair; asserts the
  batch is still there afterward (never cleared). Phase 3 reopens on a
  clean device and lets recovery actually finish, proving a later, clean
  attempt can still repair the page from that same, still-intact batch.
- `SilentlyDropsOneWrite` - a `BlockDevice` that reports every `write_at`
  call a success without ever forwarding the `n`th one to the inner
  device - modeling the "disk lies about a write having landed" failure
  ADR 0005 explicitly places outside the double-write buffer's own
  guarantees. `FaultyDevice` cannot produce this shape (its tears always
  co-occur with a reported error), so this directly exercises
  `recover_double_write`'s own verify-before-clear step: the one thing
  standing between a silently-dropped restore write and an incorrectly-
  cleared batch.
- `recover_double_write_refuses_to_clear_the_batch_when_a_restore_write_silently_fails` -
  direct test of the verify-before-clear step: even when the restore
  write reports success, `recover_double_write` must not trust that alone
  - a write that silently didn't land must still be caught by re-reading
  and re-checksumming before the batch that could still fix it is thrown
  away. Corrupts the real page's on-disk bytes directly (a body byte at
  an offset relative to the page's own slot, not the start of the file,
  since page 0 is the database header, not this page), then recovers
  against a `SilentlyDropsOneWrite`, asserting
  `StorageError::DoubleWriteRestoreFailed` and that the batch is left in
  place.
- `open_rejects_zero_capacity` - M12 part 4: `DoubleWriteBuffer::open`
  must reject a zero capacity outright rather than constructing a buffer
  `BufferPool::flush_pages`'s `pages.len() <= capacity` assertion would
  debug-panic against on the very first non-empty flush.

## Usage Example

```rust
use storage::block_device::{DurabilityModel, FaultyDevice};

let db_device = FaultyDevice::with_torn_sectors(
    Box::new(FileDevice::new(db_file)),
    counter.clone(),
    2,
    DurabilityModel::torn_write(),
    vec![0, 1, 2, 3],
);
// ... write dirty pages, flush_all() (fails - simulates a crash) ...
// reopen on a clean device:
recovery::recover_double_write(&mut disk, &mut dwb)?;
```

Run via `cargo test -p storage --test double_write_buffer`, the
mechanism-level counterpart to the full-workload crash-injection sweep in
`crates/engine/tests/crash_injection.rs`: this file proves the
double-write buffer's own protocol steps and boundary shapes in
isolation, at the `storage` layer, before that sweep proves the same
mechanism holds up under real SQL workloads and every possible crash
point.
