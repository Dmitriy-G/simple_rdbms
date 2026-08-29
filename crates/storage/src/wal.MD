# wal

The write-ahead log: `LogManager`, its on-disk record format, its segment
family, and the iterator recovery scans it with. Owns a database's
`<db_path>.wal.NNNNNN` segment files (`docs/adr/0011-segmented-write-ahead-log.md`),
assigning each appended record a monotonically increasing `Lsn` and
enforcing the write-ahead rule from the durable end - a page may not be
written to the data file until the log record covering it is durable (see
`BufferPool::flush_pages`). A torn write to the WAL never needs the
double-write buffer the way a data-file page does: `append` only
serializes into an in-memory buffer, and nothing reaches disk until
`flush`, so a crash mid-append leaves, at worst, an incomplete trailing
record that `decode_record`'s own length-prefix-plus-CRC framing detects
and `scan_segment` truncates away on the next open - never a half-written
record mistaken for a valid one. This is what makes "steal" (evicting a
dirty page before its transaction commits) and "no-force" (not flushing a
transaction's pages at commit) both safe: redo replays every logged
`Update` regardless of commit status, and undo rolls back anything that
never committed - both driven by `storage::recovery`.

**LSN is a single, ever-increasing byte offset into a virtual, unbroken log
stream - it does not change meaning just because that stream now lives in
more than one file.** A segment's own on-disk position has nothing to do
with the LSNs its records carry except through its header's `start_lsn`
field: a record's LSN is always `segment.start_lsn + (its byte offset past
that segment's header)`, for whichever segment actually holds it. This is
what lets `prev_lsn`, `undo_next_lsn`, and `last_checkpoint_lsn` - all
plain LSN values baked into already-written records - keep meaning exactly
what they meant before segmentation existed, with no rewriting needed as
segments roll over or get deleted.

**The retained segments are never "the whole history" once checkpoints run
regularly.** `LogManager::truncate_below` deletes every sealed segment a
checkpoint proves nothing can still reach (the invariant below), so `open`
rebuilding `last_lsn_by_txn` from every currently-retained segment is
bounded by how much has accumulated *since the last checkpoint that could
truncate*, not by the database's total lifetime - see
`docs/adr/0011-segmented-write-ahead-log.md` for why this, rather than
locating "the last checkpoint" directly, is what actually bounds it.

**The truncation invariant: nothing undo or redo can still reach may ever
be deleted.** `LogManager::truncate_below`'s caller (`txn::write_checkpoint`)
is responsible for passing a bound that is safe by this rule, and
`truncate_below` itself only ever deletes whole *sealed* segments strictly
below that bound - never the active segment, and never a partial segment.
See `docs/adr/0011-segmented-write-ahead-log.md` for why the bound must use
each active transaction's *earliest* LSN, not its most recent one.

## Key Components

- `CHECKPOINT_TXN` - the `txn_id` every `CheckpointBegin`/`CheckpointEnd`
  record is logged under. Reserved so it can never collide with a real,
  caller-assigned transaction id (those start at `0` and count up);
  recovery's Analysis pass special-cases this id out of the Active
  Transaction Table.
- `SEGMENT_MAGIC` - the 8-byte magic string every segment file's header
  starts with.
- `SEGMENT_HEADER_LEN`/`HEADER_LEN` - the length, in bytes, of a segment
  file's fixed header: `SEGMENT_MAGIC` (8 bytes) plus the segment's
  `start_lsn` (8 bytes, little-endian). `HEADER_LEN` is the public name
  (unchanged since before segmentation existed, so every caller - and the
  LSN-space math below - keeps calling it that) for the same constant. No
  real record can ever start before this offset within its own segment,
  which is what lets `Lsn(0)` - and, defensively, anything less than
  `HEADER_LEN` - be treated as "no record" rather than a real one: every
  segment's own first record starts exactly at `start_lsn`, and `start_lsn`
  for the very first segment a database ever creates is `HEADER_LEN`
  itself.
- `DEFAULT_SEGMENT_SIZE` - 16 MiB. The body-size threshold `flush` checks
  after every durable write to decide whether the active segment should be
  sealed and a new one opened (`LogBufferInner::roll_segment`). Not a hard
  cap - a single flush can still land a batch that pushes a segment past
  this by however much was buffered - just the trigger for rolling before
  the *next* one.
- `SegmentStore` - a trait: opens (creating if necessary) and removes the
  on-disk devices backing one segment of a segmented log, and lists which
  segment ids currently exist. `existing_segments(&self) ->
  Result<Vec<u64>, StorageError>`, `open(&self, id) ->
  Result<Box<dyn BlockDevice>, StorageError>`, `remove(&self, id) ->
  Result<(), StorageError>`. The seam between "how a segment's bytes are
  addressed" and everything else in this module, so `LogManager` itself
  never has to know whether a segment is a real file or something else.
- `segment_path(base, id) -> PathBuf` - the file-family naming scheme:
  `base` with `.NNNNNN` (a zero-padded, unbounded-width decimal id)
  appended. Exposed (not just used internally by `FileSegmentStore`) so a
  test that needs to reach into a specific segment's raw bytes - flipping
  one to prove CRC detection still works, say - can compute its path
  without duplicating the format string.
- `FileSegmentStore` - the real, filesystem-backed `SegmentStore`
  `LogManager::open` uses: `existing_segments` lists the parent directory
  and parses every sibling whose name is `<base's file name>.` plus
  exactly six ASCII digits; `open` opens (creating if needed) the segment
  file at `segment_path(base, id)`; `remove` deletes it, treating "already
  gone" as success rather than an error, since `truncate_below` racing a
  half-completed prior truncation should not itself fail.
- `NoSegmentStore` - private. The `SegmentStore` `LogManager::open_with_device`
  installs behind its single, caller-supplied device: `existing_segments`
  reports none, `open` refuses (there is no second segment to open - a
  device-backed log never rolls, so this is never actually called), and
  `remove` is a harmless no-op (never called either, since nothing is ever
  sealed).
- `write_segment_header(device, start_lsn)`/`read_segment_header(device) ->
  Result<u64, StorageError>` - private. Write/read a segment's fixed
  16-byte header (`SEGMENT_MAGIC` then `start_lsn`, little-endian).
  `write_segment_header` truncates the device to empty first, so it is
  always safe to call on a brand-new or freshly-reinitialized device
  regardless of what garbage bytes it may already contain past the header.
- `scan_segment(device, start_lsn) -> Result<(u64, HashMap<TxnId, u64>), StorageError>` -
  private. Reads one segment's entire body (bounded by that segment's own
  size, never the whole log) into memory, decodes every complete,
  CRC-valid record forward from `SEGMENT_HEADER_LEN`, and truncates away
  anything past the last valid one (a torn write from a crash mid-append) -
  the same truncate-the-tail behavior the pre-segmentation single-file
  `open` had, now scoped to one segment. Returns the LSN one past the last
  record found (relative to `start_lsn`) and every transaction's
  most-recently-seen LSN *within this segment* - `bootstrap` merges these
  contributions across every segment it scans, so a transaction whose
  chain spans a segment boundary still ends up with its true, most recent
  LSN once every retained segment has been folded in.
- `read_record_at(device, offset) -> Result<Option<LoggedRecord>, StorageError>` -
  private. Reads exactly one record from `device` at a *local* (already
  segment-relative) byte offset: one read for its `total_len` prefix, then
  one more sized to exactly that many bytes - the same two-read shape
  `read_at` always used, now shared between the durable-region branches of
  `read_at` and `LogManager::read_at` itself no longer needing to
  duplicate it per segment.
- `SegmentMeta` - private. One sealed segment's identity: `id` (its
  position in the `.NNNNNN` sequence) and `start_lsn`. Deliberately does
  *not* hold an open device handle - a sealed segment is reopened lazily,
  through `store`, only when something actually needs to read it
  (`iter_from`, `read_at`, `bootstrap`), so a long-lived database with many
  sealed segments does not pay for dozens of idle open file handles.
- `MIN_RECORD_LEN` - the fixed-size portion of a serialized record: front
  `total_len` (4 bytes), `lsn` (8), `prev_lsn` (8), `txn_id` (8), `kind`
  tag (1), `crc32` (4), and trailing `total_len` (4). Any candidate
  record shorter than this cannot be real, regardless of what its
  `total_len` field claims.
- `LogRecordKind` - the kind-specific payload of a log record, and the
  physical before/after images needed to redo or undo it. `Update` is the
  one workhorse variant: a byte-range write to any page, whether that
  page is a heap page's slot array, a catalog row, or (later) a B+tree
  node - the log does not need to know which. `Begin` marks the start of
  a transaction. `Commit` marks it committed; its updates must survive a
  crash from this point on. `Abort` marks it aborted; its updates must be
  undone. `End` marks that every record belonging to a transaction (redo
  and undo) has been fully processed; recovery need not look at it again.
  `Update { page_id, offset, before, after }` is a physical byte-range
  write. `AllocPage { page_id }` records that `page_id` was allocated,
  extending the file. `Clr { page_id, offset, after, undo_next_lsn }` is a
  compensation log record, written while undoing an `Update`, so that a
  second crash during undo does not undo the same change twice -
  `undo_next_lsn` points at the next record still to be undone for this
  transaction, skipping over the one this CLR compensates for.
  `CheckpointBegin` marks the start of a fuzzy checkpoint, logged under
  `CHECKPOINT_TXN`; its own LSN is recorded in the page-0 header so
  recovery's Analysis pass knows where to start scanning. `CheckpointEnd
  { att, dpt }` carries the Active Transaction Table and Dirty Page Table
  snapshots captured at (approximately) the matching `CheckpointBegin`,
  logged under `CHECKPOINT_TXN`.
- `LogRecordKind::tag(&self) -> u8` - private. The kind's on-disk tag byte
  (`Begin` = 0 through `CheckpointEnd` = 8).
- `LogRecordKind::encode_payload(&self, buf)` - private. Appends this
  kind's payload bytes to `buf`; called by `encode_record`.
- `LogRecordKind::decode_payload(tag, bytes) -> Option<Self>` - private.
  Decodes a payload given its `tag` byte, or `None` if the bytes are too
  short, too long, or the tag is unrecognized - any of which means this
  candidate record is not a real one (its CRC should already have been
  checked by the caller before this is trusted).
- `LogRecord` - a record ready to append: the transaction it belongs to
  and its kind. `LogManager::append` assigns the LSN and fills in
  `prev_lsn` by chaining from that transaction's most recently appended
  record, so callers never need to track it themselves.
- `LoggedRecord` - a record read back from the log, with the envelope
  fields `LogManager::append` assigned it at write time: `lsn`,
  `prev_lsn` (`None` if this is the transaction's first record), `txn_id`,
  `kind`.
- `Cursor` - private. A minimal, panic-free byte reader: every `take*`
  call either returns the requested field or `None`, never panics on a
  short or malformed buffer. `take(n)`, `take_u16()`, `take_u32()`,
  `take_u64()`, `take_len_prefixed()` (a `u32` length prefix followed by
  that many bytes), and `exhausted()` (`true` if every byte has been
  consumed - a malformed payload with extra trailing bytes the tag's
  decoder didn't account for fails this).
- `encode_record(lsn, prev_lsn, txn_id, kind) -> Vec<u8>` - private.
  Serializes into a complete, self-describing record: `total_len | lsn |
  prev_lsn | txn_id | kind | payload | crc32 | total_len`. `prev_lsn` of
  `None` is encoded as `0`, which is safe because LSNs are assigned
  starting at `1`.
- `decode_record(bytes, pos) -> Option<(LoggedRecord, usize)>` - private.
  Decodes the record starting at `bytes[pos..]`, returning it along with
  the number of bytes it occupies, or `None` if there is no complete,
  CRC-valid record there - either because fewer than `total_len` bytes
  remain (a torn write from a crash mid-append) or because the CRC does
  not match (a flipped or corrupted byte). Both cases are reported
  identically: recovery treats a crash mid-append as normal, not as
  corruption to raise an error over.
- `LogBufferInner` - private. Everything `LogManager` needs a mutex around
  as one unit: `store` (the `Arc<dyn SegmentStore>` behind every segment
  this log opens, `Arc` rather than `Box` so `iter_from`/`read_at` can
  clone a handle out and open older segments without holding this mutex
  across that I/O), `target_segment_size` (`u64::MAX` for a
  device-backed, never-rolling log), `sealed` (`Vec<SegmentMeta>`,
  ascending by id, every segment that has been rolled past but not yet
  truncated), `active_id`/`active_start_lsn`/`active_device` (the segment
  every `append`/`flush` targets), an in-memory `buffer` of records
  appended but not yet flushed, `next_lsn` (the LSN the next `append` will
  assign - the active segment's durable length plus buffer length, offset
  by `active_start_lsn`), `last_lsn_by_txn` (each transaction's most
  recently appended LSN, consulted by `append` to fill in `prev_lsn`), and
  `bytes_appended` (cumulative, for the checkpoint byte-threshold
  trigger). Grouped into one struct rather than one field each because
  every one of them changes together on an `append`, a `flush`, or a
  segment roll - there is no scenario where taking the lock for just one
  of them makes sense.
  - `LogBufferInner::roll_segment(&mut self) -> Result<(), StorageError>` -
    private. Seals the current active segment (pushes its `id`/`start_lsn`
    into `sealed`), opens a fresh segment one id higher via `store`, writes
    its header with `start_lsn = next_lsn` (the byte the very next `append`
    will land at), and makes it the new active segment. Only ever called
    right after a successful `flush` has already `sync_all`'d the segment
    being sealed, so nothing ever enters `sealed` with an unsynced tail.
- `LogManager` - `Mutex<LogBufferInner>` plus a standalone `AtomicU64
  durable_lsn` (M10.1, `docs/ROADMAP.md`): every read of `durable_lsn` -
  above all `flush`'s own early-return check, the hottest read in the
  type - is a lock-free atomic load instead of a mutex acquisition, since
  it's consulted on every page flush (`BufferPool::flush_pages`) even
  when nothing about the buffer or file needs touching.
- `LogManager::open(path)` - opens (creating the family if necessary) the
  segmented log at `path`, using `FileSegmentStore` and
  `DEFAULT_SEGMENT_SIZE`.
- `LogManager::open_with_segment_size(path, target_segment_size)` -
  `#[cfg(any(test, feature = "test-util"))]`. Same as `open`, but with a
  caller-chosen roll threshold instead of `DEFAULT_SEGMENT_SIZE` - the only
  way a test can actually exercise rollover/truncation without appending
  16 MiB of records first.
- `LogManager::open_with_segment_store(store, target_segment_size)` -
  `#[cfg(any(test, feature = "test-util"))]`. Same as `open_with_segment_size`,
  but with a caller-supplied `SegmentStore` instead of a real
  `FileSegmentStore` - lets a test wrap every segment device it opens
  (e.g. in a byte-counting `BlockDevice`) to observe how much of the log
  a given operation actually touches.
- `LogManager::segment_ids(&self) -> Vec<u64>` -
  `#[cfg(any(test, feature = "test-util"))]`. Every segment id this log
  currently has open or sealed, active id last. Test-only introspection
  for asserting a truncation actually dropped the segments it should have.
- `LogManager::open_with_device(device)` - opens a log backed by an
  arbitrary `BlockDevice` as segment `0` of a log that never rolls
  (`NoSegmentStore`, `target_segment_size = u64::MAX`), for tests and
  crash-injection fault wrapping that hand this a single, already-constructed
  device (`docs/adr/0011-segmented-write-ahead-log.md` on why this stays a
  single-segment legacy path rather than growing a multi-device factory
  API). Delegates straight to `bootstrap` with an empty sealed-id list.
- `LogManager::open_with_store(store, target_segment_size)` - private.
  Lists `store.existing_segments()`; the highest id found (or `0`, for a
  brand-new database) becomes the active segment, and every other id found
  becomes a sealed candidate `bootstrap` will fold in.
- `LogManager::bootstrap(store, target_segment_size, active_id, active_device, sealed_ids)` -
  private, the shared core both `open_with_device` and `open_with_store`
  funnel through. For each sealed id: opens it, reads its header for
  `start_lsn`, `scan_segment`s it (bounded to that one segment's size) to
  fold its transactions' last-seen LSNs into `last_lsn_by_txn`, and records
  its `SegmentMeta`. For the active segment: if it is shorter than
  `SEGMENT_HEADER_LEN` (a brand-new or torn-at-birth device - the same
  "nothing could have been durably logged yet either way" reasoning the
  pre-segmentation single-file `open` used, since a segment header is
  written exactly once, before any record could possibly have been
  appended to it), writes a fresh header at `HEADER_LEN` and starts
  `next_lsn` there; otherwise reads its existing header and `scan_segment`s
  it to find the true append point and fold in its own contributions. This
  is the one place in the module - not `iter_from`, not `read_at` - that
  ever reads more than one segment's worth of bytes, and per the truncation
  invariant above, "every currently-retained segment" is bounded by how
  recently a checkpoint last ran, not by the database's total history.
- `LogManager::append(&self, record) -> Result<Lsn, StorageError>` -
  appends `record` to the log, returning its assigned `Lsn`: its byte
  offset in the log stream, i.e. the durable file length plus the current
  in-memory buffer length. Only buffers the bytes in memory; call `flush`
  to force them to disk. Emits a `trace!` naming the assigned `lsn` and
  `txn_id` after appending - built entirely from values already computed
  for the return value, so the hot path pays nothing extra when this
  level is disabled. Increments `wal_bytes_written_total` (a `metrics`
  counter - see `docs/ROADMAP.md`'s M13 entry) by the encoded record's
  byte length; despite the name this counts bytes buffered, not yet
  necessarily fsync'd - `flush`'s own counter is what tracks durability.
- `LogManager::flush(&self, up_to)` - forces every buffered record up
  to and including the one starting at `up_to` to disk. Idempotent:
  returns immediately if that record is already durable, via the lock-free
  `durable_lsn` atomic - no mutex is even acquired on the early-return
  path. The check is `durable_lsn > up_to.0`, strictly - not `>=`: `up_to` is a record's
  *start* offset, and `durable_lsn` is byte-granular now, so `durable_lsn
  == up_to.0` means the durable region ends exactly where that record
  begins - none of it has reached disk yet, and a real flush is still
  required. `durable_lsn` only ever advances in whole-record increments
  (every flush durable-izes the entire buffer at once), so `durable_lsn >
  up_to.0` correctly implies the *whole* record at `up_to` is durable, not
  just its first byte. Times the `sync_all` call specifically (not the
  preceding buffered `write_at`), incrementing `wal_fsync_total` and
  recording `wal_fsync_duration_seconds` - per `docs/ROADMAP.md`'s M13
  entry, "commit latency lives here," since `TransactionManager::commit`
  calls this synchronously before acknowledging a commit. After the sync,
  checks whether the active segment's body has reached
  `target_segment_size` and, if so, calls `LogBufferInner::roll_segment` -
  always *after* the sync, so a segment only ever gets sealed once every
  byte it holds is confirmed durable.
- `LogManager::flush_all(&self)` - forces every record appended so
  far to disk, regardless of what any individual page needs. Used at
  clean shutdown.
- `LogManager::durable_lsn(&self) -> Lsn` - the offset one past the last
  durably-written byte.
- `LogManager::bytes_appended(&self) -> u64` - cumulative bytes appended
  since this `LogManager` was opened.
- `LogManager::last_lsn_for(&self, txn_id) -> Option<Lsn>` - `txn_id`'s
  most recently appended LSN, or `None` if it has never appended a
  record. This is the authoritative source for a transaction's tail of
  the log - unlike a value cached at `begin` time, it reflects every
  record appended since, including ones appended through paths that never
  see a `TransactionManager`, such as `PageWriteGuard::write`'s own
  `Update` records.
- `LogManager::max_txn_id(&self) -> Option<TxnId>` - the highest
  transaction id that appears anywhere in `last_lsn_by_txn` (excluding
  `CHECKPOINT_TXN`), or `None` if the log has never recorded a real
  transaction. Since transaction ids are assigned monotonically and never
  reused, the highest one can only ever live in the most recently written
  segments - the ones `truncate_below` never removes - so this stays
  correct even though `last_lsn_by_txn` itself no longer reflects the
  database's *entire* history once older segments have been truncated
  away; see `docs/adr/0011-segmented-write-ahead-log.md`.
- `LogManager::truncate_below(&self, bound) -> Result<(), StorageError>` -
  deletes every whole *sealed* segment strictly below `bound` (the
  truncation invariant above), never the active one. Computes which
  segments qualify under the mutex (a segment's own end is the next
  segment's `start_lsn`, or `active_start_lsn` for the last sealed one),
  clones `store` out, then removes each qualifying id's device *after*
  releasing the mutex - deletion is real I/O and must not happen while
  holding the lock other operations need. If a `remove` call fails partway
  through a batch, the segments already dropped from `sealed` stay
  dropped (their metadata is gone) even though their files may still exist
  on disk - an orphaned file, not a correctness problem, since nothing
  ever looks for a segment id `sealed` doesn't know about.
- `LogManager::iter_from(&self, from) -> Result<LogIterator, StorageError>` -
  returns a forward iterator over every record with LSN `>= from`,
  spanning however many segments that requires - whether durably on disk
  or still sitting in the not-yet-flushed in-memory buffer. The buffer's
  bytes are exactly what `flush` would write immediately after the active
  segment's durable region, so appending them here gives the same stream
  `flush`-then-read would - which is what lets a runtime `abort` (see
  `txn::TransactionManager::abort`) find and undo a transaction's own
  records before they have ever been flushed, not just after a crash.
  Finds every sealed segment whose `start_lsn <= from` (there can be more
  than one still to visit - `from` narrows *which* segment to start
  from, but every later segment must still be scanned up to the current
  tail) under the mutex, eagerly reads only the active segment's durable
  bytes plus the buffer (bounded to one segment's worth, not the whole
  tail `from` might otherwise imply), and hands the rest off to
  `LogIterator` to load lazily. `from` must be `HEADER_LEN` or an LSN some
  earlier `append` actually returned - i.e. a real record boundary within
  whichever segment holds it - since `LogIterator` always starts decoding
  each segment from its own header end (a boundary by construction) and
  simply skips any record whose LSN is still `< from`, rather than seeking
  to a computed mid-segment offset.
- `LogManager::read_at(&self, lsn) -> Result<Option<LoggedRecord>, StorageError>` -
  reads and decodes exactly one record by its LSN, without scanning any
  other part of the log: `undo_transaction` uses this to walk a
  transaction's chain backward one record at a time, which is what keeps
  undoing N records `O(N)` rather than the `O(N^2)` that
  `iter_from(lsn).next()` used to cost (each call re-reading the whole log
  just to yield its first matching record). Returns `None` for `Lsn(0)`
  (the "no record" sentinel) or a torn/invalid record. Three cases, in
  order: `lsn` at or past `durable_lsn` decodes straight out of the
  in-memory buffer, no device I/O at all; `lsn` at or past the active
  segment's `start_lsn` reads it (via `read_record_at`) from the active
  device at the corresponding local offset; otherwise a linear search
  through `sealed` (in practice a handful of entries) finds whichever
  segment's `start_lsn` is the largest one `<= lsn`, opens it through
  `store`, and reads from there. A durable-region lookup always costs
  exactly two device reads (peek `total_len`, then read exactly that many
  bytes) rather than one read sized to the whole durable region.
- `LogIterator` - forward iterator over `LoggedRecord`s spanning however
  many segments a call to `LogManager::iter_from` needs: `pending_segments`
  (ids still to open, oldest first), `tail` (the active segment's durable
  bytes plus buffer, visited last), and `current`/`pos` (the segment
  currently being decoded). `next()` decodes forward through `current`,
  skipping anything below `from`; once `current` is exhausted it opens the
  next pending segment (via the cloned `store` handle, outside any lock)
  or, once there are none left, moves on to `tail`. This is what "streams
  segment by segment" means in practice: at most one segment's bytes are
  ever resident at once, never the whole span from `from` to now. The one
  real trade-off is that `Iterator::next` cannot return a `Result`: if
  opening or reading a *later* pending segment fails, iteration simply
  ends (as `None`) rather than surfacing the I/O error the way `iter_from`
  itself still does for its own eager read of the tail -
  `docs/adr/0011-segmented-write-ahead-log.md` has the full reasoning for
  why that narrowing is acceptable here.

## Usage Example

```rust
use storage::wal::{LogManager, LogRecord, LogRecordKind};

let mut log = LogManager::open("test.db.wal")?;
let lsn = log.append(LogRecord { txn_id: common::TxnId(1), kind: LogRecordKind::Begin })?;
log.flush(lsn)?;
for record in log.iter_from(lsn)? {
    println!("{:?}", record.kind);
}
```

`BufferPool` is the only ordinary caller: `PageGuard::write`/`stamp_write`
call `append_log` (which wraps `LogManager::append`) before touching a
page's bytes, `flush_pages` calls `flush` to enforce the write-ahead rule
before a batch reaches the double-write buffer, and `flush_log_all` calls
`flush_all` at clean shutdown. `storage::recovery` is the other caller,
walking the log forward via `log_iter_from`/`iter_from` during Analysis
and Redo, and backward one record at a time via `read_log_at`/`read_at`
during Undo. `txn::write_checkpoint` calls `BufferPool::truncate_log_below`
(which wraps `truncate_below`) once a checkpoint's records are durable,
with the bound `docs/adr/0011-segmented-write-ahead-log.md` derives -
this is the one call site that ever shrinks the log rather than only
growing it.
