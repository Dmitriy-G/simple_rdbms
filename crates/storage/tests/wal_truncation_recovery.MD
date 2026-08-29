# wal_truncation_recovery

Regression coverage for the round-2 critical bug where `LogManager::iter_from`
treated "no sealed segment starts at or before `from`" as "nothing to read"
instead of "start from the beginning of what's retained" (`wal.MD`'s
truncation-invariant section). Two tests, at two different altitudes: one
pins the exact mechanism inside `LogManager` directly, the other reproduces
the end-to-end failure mode against a full `BufferPool` plus
`storage::recovery`, without ever letting the on-disk checkpoint LSN get
ahead of what `truncate_below` has actually deleted.

Both tests live in `crates/storage` rather than `crates/engine` because the
bug is entirely a `storage` concern — `crates/engine`'s `Database` is not on
this crate's dependency list (`CLAUDE.md`'s dependency-edge rules), so
neither test reaches for it; everything is built from `BufferPool`,
`LogManager`, and `storage::recovery` directly, the same way
`wal_segments.rs`'s own recovery test does.

## Key Components

- `SMALL_SEGMENT` - a 512-byte roll threshold, small enough that a few
  hundred small records reliably spans several segments without waiting on
  `DEFAULT_SEGMENT_SIZE`'s 16 MiB.
- `open_pool(dir, target_segment_size) -> Result<BufferPool, Box<dyn Error>>` -
  assembles a real, file-backed `BufferPool` (`test.db`, `test.db.dwb`,
  `test.db.wal` under `dir`) with a caller-chosen segment size, mirroring
  `wal_segments.rs`'s helper of the same name.
- `filler(log, txn_id) -> Result<Lsn, Box<dyn Error>>` - appends and flushes
  one small `Update` record under `txn_id`, returning its LSN. Used to drive
  a `LogManager` through several segment rolls without needing a real page
  or buffer pool.
- `commit(pool, txn_id) -> Result<Lsn, Box<dyn Error>>` - appends and flushes
  a `Commit` record for `txn_id` against a `BufferPool`, returning its LSN.
  Since `flush_log` flushes everything buffered up to and including that
  LSN, this also durably flushes whatever `Update` record preceded it in the
  same transaction.
- `segment_start_lsn(wal_path, id) -> Result<u64, Box<dyn Error>>` - reads a
  sealed segment's file directly (bytes 8..16, right after
  `SEGMENT_MAGIC`) and decodes its `start_lsn`, the same on-disk layout
  `wal.MD` documents for `write_segment_header`/`read_segment_header`. Used
  to establish, independently of the code under test, exactly where the
  earliest surviving segment begins.

## Usage Example

```rust
let dir = tempfile::tempdir()?;
let log = LogManager::open_with_segment_size(dir.path().join("test.wal"), 512)?;
for i in 0..400u64 {
    filler(&log, TxnId(i % 5))?;
}
log.truncate_below(some_bound)?;
let records: Vec<_> = log.iter_from(Lsn(HEADER_LEN))?.collect();
```

`recovery_reads_sealed_segments_when_the_header_lsn_predates_them` is the
direct regression for the mechanism: it drives a bare `LogManager` through
enough small records to seal several segments, truncates below a bound that
leaves at least one sealed segment behind, and asserts that
`iter_from(Lsn(HEADER_LEN))` - a `from` that precedes every currently
retained segment, exactly the input that used to make `rposition` return
`None` - still starts from the earliest surviving segment rather than
skipping straight to the active one. It checks this two ways: the lowest
LSN returned must equal the earliest surviving segment's own `start_lsn`
(read independently off disk), and the total record count must equal
exactly how many of the appended LSNs are still `>=` that segment's start -
not merely "some records came back," which the pre-fix code would also have
satisfied by reading the active segment alone.

`a_crash_after_truncation_still_redoes_committed_work` is the end-to-end
version: it commits a first row and flushes the page directly (making that
row durable through the data file, the ordinary and correct reason a real
checkpoint would ever be allowed to truncate the segments describing it),
then writes two hundred more committed rows - each at its own offset, never
overwriting the last - spanning several segment rolls, then a final row,
then truncates using the dirty page table's own earliest recLSN, exactly
the bound a real checkpoint would compute. Truncation never touches the
page-0 header - no checkpoint LSN is ever written - so the effective
recovery start point stays `HEADER_LEN` (16), the "page 0 never reached
disk at all" branch the bug report calls out explicitly. Before reopening,
it reads the on-disk header directly (bytes 32..40 of `test.db`, `disk.MD`'s
`LAST_CHECKPOINT_LSN_RANGE`) and asserts this precondition explicitly
rather than assuming it - the same discipline the R-1 brief itself
demanded, so a change to the workload sizing can't quietly turn this into a
test that passes without ever exercising the bug.

The one row it asserts on by index - `mid_index`, chosen as the latest of
the two-hundred middle rows whose commit LSN is still below the *active*
segment's own `start_lsn` - is the crux of the test: an earlier version of
this test asserted on the *last* row written instead, which happened to
still be sitting in the active segment (always scanned regardless of the
bug) and so passed even with the pre-fix code reinstated. Only a row whose
sole surviving record lives in a *sealed but retained* segment actually
exercises `iter_from`'s walk through `pending_segments`; the test computes
`active_start_lsn` from the real, on-disk active segment header rather than
assuming which iteration that is, since exactly how many rows land in the
active segment depends on segment-roll timing this test does not control
precisely. It then reopens a fresh `BufferPool` over the same files, calls
`storage::recovery::recover`, and asserts the first row (durable via the
direct flush), the last row, and `mid_index`'s row are all present. Without
the `unwrap_or(0)` fix, `mid_index`'s row silently reads back as zero: its
record lived in a segment `iter_from` never visited.
