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
and `scan_active_segment` truncates away on the next open - never a
half-written record mistaken for a valid one. This is what makes "steal" (evicting a
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

**`iter_from` must never read less of the log than `from` asks for.** If no
sealed segment's `start_lsn` is `<= from` - meaning `from` precedes every
segment this log still has - the correct segment to start from is the
*first* surviving one, not "none": everything still retained is at or
after `from` in that case, and skipping straight to the active segment
would silently drop every sealed segment's records. Reading more of the
log than strictly necessary only costs startup time; reading less costs
committed data. Never resolve that ambiguity in the direction that reads
less. This matters because `from` is not always trustworthy: it usually
comes from the page-0 header's `last_checkpoint_lsn`, and page 0 is
recovered by ordinary buffer-pool eviction like any other page, so a
crash can leave the on-disk header behind whatever `truncate_below` has
already deleted (`txn::write_checkpoint` flushes and syncs page 0 before
truncating specifically to keep this from happening in practice - see
`checkpoint.MD` - but `iter_from` does not get to assume that always
succeeded).

**A sealed segment is immutable; opening a database must not write to
one.** `scan_active_segment` may trim a torn trailing write with `set_len`
- that is only ever correct for the *active* segment, the one still being
appended to, where a torn tail really is just an incomplete write from the
last crash. `scan_sealed_segment` validates the same bytes and never
writes: if decoding does not reach the segment's full recorded length, that
is corruption in a file nothing should still be extending, and it returns
`StorageError::CorruptLogHeader` naming the segment and the offset the
decode stopped at. Before this split, `bootstrap` ran the trimming scan
against every sealed segment too, so a corrupt byte anywhere in one of
them silently truncated it and dropped everything after that point - and
because `bootstrap` never kept a sealed segment's post-scan length, nothing
noticed the resulting hole in the LSN space.

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
- `FaultySegmentStore` - `#[cfg(any(test, feature = "test-util"))]`. A
  `SegmentStore` that wraps a real `FileSegmentStore` and hands back every
  segment device wrapped in a `block_device::FaultyDevice`, all sharing one
  `counter`/`fail_at`/`DurabilityModel`. There is exactly one on-disk log
  layout (the paragraph above) - a numbered segment family - so this is the
  only way a crash-injection harness gets fault injection into the log at
  all; there is no single-device constructor left to hand it a lone faulty
  device instead. `open(id)` wraps *that specific segment's* device freshly
  on every call rather than wrapping once at construction, which matters
  because segments are opened lazily by id as the log rolls - a harness
  that only wrapped the first segment would silently stop injecting faults
  into every segment after the first roll. Lives here rather than in
  `crates/storage/tests/support/` because `mod support;` only resolves
  within the crate whose `tests/` directory declares it - `engine` and
  `catalog`'s own crash-injection harnesses need this too, and a Rust test
  module can't reach into another crate's `tests/` directory, so a helper
  meant to be shared *across* crates has to be part of the library itself,
  gated the same way `open_with_segment_store` already is.
- `write_segment_header(device, start_lsn)`/`read_segment_header(device) ->
  Result<u64, StorageError>` - private. Write/read a segment's fixed
  16-byte header (`SEGMENT_MAGIC` then `start_lsn`, little-endian).
  `write_segment_header` truncates the device to empty first, so it is
  always safe to call on a brand-new or freshly-reinitialized device
  regardless of what garbage bytes it may already contain past the header,
  then ends with `device.sync_all()`: a segment header is a durability
  boundary exactly like any record's, not a convenience, since everything
  this module does to address a record by LSN depends on the segment that
  holds it having a header whose `start_lsn` actually reached disk. A
  power loss between `roll_segment` making a new segment active and its
  header's bytes actually landing would otherwise leave that segment
  looking headerless on the next open.
- `scan_decoded_records(device, start_lsn) -> Result<(u64, usize, HashMap<TxnId, u64>), StorageError>` -
  private. The shared read-and-decode walk `scan_active_segment` and
  `scan_sealed_segment` both build on: reads one segment's entire body
  (bounded by that segment's own size, never the whole log) into memory
  and decodes every complete, CRC-valid record forward from
  `SEGMENT_HEADER_LEN`. Returns the LSN one past the last record found
  (relative to `start_lsn`), the byte offset that last record ends at, and
  every transaction's most-recently-seen LSN *within this segment* -
  `bootstrap` merges these contributions across every segment it scans, so
  a transaction whose chain spans a segment boundary still ends up with
  its true, most recent LSN once every retained segment has been folded
  in. Never touches the device itself; deciding what a short decode
  *means* - a normal torn tail, or corruption - is entirely the caller's
  job, which is exactly why the two callers below exist as separate
  functions rather than one with a boolean flag.
- `scan_active_segment(device, start_lsn) -> Result<(u64, HashMap<TxnId, u64>), StorageError>` -
  private. Calls `scan_decoded_records`, then truncates away anything past
  the last valid record with `device.set_len` - a torn write from a crash
  mid-append, the same truncate-the-tail behavior the pre-segmentation
  single-file `open` had, now scoped to one segment. Only ever correct for
  the segment still being appended to.
- `scan_sealed_segment(device, start_lsn) -> Result<(u64, HashMap<TxnId, u64>), StorageError>` -
  private. Calls `scan_decoded_records`, then *validates* rather than
  trims: if the last decoded record's end offset does not equal the
  segment's actual on-disk length, returns `StorageError::CorruptLogHeader`
  naming `start_lsn` and both offsets, and never calls `device.set_len` at
  all. A sealed segment's tail should only ever be short because the log
  genuinely truncated it there (`truncate_below` deletes whole segments,
  never leaves a partial one) - if it decodes short for any other reason,
  that is corruption in a file nothing should still be extending, not a
  normal crash artifact to clean up silently.
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
  `LogBufferInner::sealed_device_cache` remembers the single
  most-recently-opened one across calls, so a caller that keeps reading
  the same sealed segment - `undo_transaction` walking a `prev_lsn` chain
  backward one `read_at` at a time is the case this exists for - does not
  reopen it once per record.
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
  recently appended LSN, consulted by `append` to fill in `prev_lsn`),
  `bytes_appended` (cumulative, for the checkpoint byte-threshold
  trigger), and `sealed_device_cache` (`Option<(u64, Box<dyn
  BlockDevice>)>` - the id and open device handle of the sealed segment
  `read_at` most recently opened, or `None`). Grouped into one struct
  rather than one field each because every one of them changes together
  on an `append`, a `flush`, or a segment roll - there is no scenario
  where taking the lock for just one of them makes sense.
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
- `reject_legacy_single_file_log(base) -> Result<(), StorageError>` -
  private. Returns `StorageError::CorruptLogHeader` naming `base` if a
  plain file already exists at exactly that path, before either `open`
  variant below ever consults a `SegmentStore`. There is exactly one
  on-disk log layout - a numbered segment family - so a bare `<path>` file
  can only be a pre-segmentation database's log; without this check
  `FileSegmentStore::existing_segments` would simply find no `.NNNNNN`
  siblings, `open_with_store` would create a brand-new, empty
  `<path>.000000`, and the entire log that file describes would be
  silently ignored rather than refused (`docs/adr/0011-segmented-write-ahead-log.md`).
- `LogManager::open(path)` - opens (creating the family if necessary) the
  segmented log at `path`, using `FileSegmentStore` and
  `DEFAULT_SEGMENT_SIZE`, after `reject_legacy_single_file_log`.
- `LogManager::open_with_segment_size(path, target_segment_size)` -
  `#[cfg(any(test, feature = "test-util"))]`. Same as `open`, but with a
  caller-chosen roll threshold instead of `DEFAULT_SEGMENT_SIZE` - the only
  way a test can actually exercise rollover/truncation without appending
  16 MiB of records first.
- `LogManager::open_with_segment_store(store, target_segment_size)` -
  `#[cfg(any(test, feature = "test-util"))]`. Same as `open_with_segment_size`,
  but with a caller-supplied `SegmentStore` instead of a real
  `FileSegmentStore` - lets a test wrap every segment device it opens
  (e.g. in a byte-counting `BlockDevice`, or `FaultySegmentStore`) to
  observe or fault-inject how much of the log a given operation actually
  touches. This is the only way to open a `LogManager` at all when the
  caller cannot hand it a real filesystem path - there is no
  single-device constructor left; a caller with one already-constructed
  fault-injecting device wraps it in a `SegmentStore` of its own (or uses
  `FaultySegmentStore`) rather than the log growing a second on-disk
  shape to accommodate it.
- `LogManager::segment_ids(&self) -> Vec<u64>` -
  `#[cfg(any(test, feature = "test-util"))]`. Every segment id this log
  currently has open or sealed, active id last. Test-only introspection
  for asserting a truncation actually dropped the segments it should have.
- `LogManager::open_with_store(store, target_segment_size)` - private.
  Lists `store.existing_segments()`; the highest id found (or `0`, for a
  brand-new database) becomes the active segment, and every other id found
  becomes a sealed candidate `bootstrap` will fold in.
- `LogManager::bootstrap(store, target_segment_size, active_id, active_device, sealed_ids)` -
  private, the shared core `open_with_store` funnels through. For each
  sealed id, in ascending order: opens it, reads its header for
  `start_lsn`, `scan_sealed_segment`s it (bounded to that one segment's
  size, and refusing rather than trimming if it does not decode cleanly to
  its full length - the invariant above) to fold its transactions'
  last-seen LSNs into `last_lsn_by_txn`, records its
  `SegmentMeta`, and remembers that segment's own `next_lsn` (the byte one
  past its last valid record) - overwritten on every iteration, so what
  survives the loop is the *last* sealed segment's `next_lsn`, i.e. the
  highest one, exactly where a well-formed active segment's own
  `start_lsn` should be. For the active segment: if it is shorter than
  `SEGMENT_HEADER_LEN`, its header write never reached disk - torn at
  birth, or (per `write_segment_header`'s own `sync_all`, above) a crash
  during `roll_segment` right after the segment was made active. Which one
  it was matters: if no sealed segment exists, this is a brand-new
  database and nothing could have been durably logged yet either way, so
  starting fresh at `HEADER_LEN` is correct - the pre-segmentation
  single-file `open` used the same reasoning. If a sealed segment *does*
  exist, starting fresh at `HEADER_LEN` would be the round-2 bug this
  guards against: it would let a brand-new record collide with LSNs the
  surviving sealed segments already used, corrupting every `prev_lsn`/
  `undo_next_lsn` chain that crosses the reused range. So instead the
  remembered last sealed `next_lsn` becomes the active segment's
  `start_lsn` too, a `warn!` is emitted (this only happens after an
  unclean shutdown, worth an operator's attention), and a fresh header
  carrying that `start_lsn` is written and synced before returning.
  Otherwise (a real header already present) reads it and
  `scan_active_segment`s it to find the true append point and fold in its
  own contributions. This is the one place in the module - not `iter_from`,
  not `read_at` - that ever reads more than one segment's worth of bytes,
  and per the truncation invariant above, "every currently-retained
  segment" is bounded by how recently a checkpoint last ran, not by the
  database's total history.
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
  ever looks for a segment id `sealed` doesn't know about. Also clears
  `sealed_device_cache` if it holds a handle to one of the ids being
  removed, still under the same mutex acquisition that computes `removed`
  - so a cached device is never held open (or, worse, read from) after
  its underlying file has been deleted.
- `LogManager::iter_from(&self, from) -> Result<LogIterator, StorageError>` -
  returns a forward iterator over every record with LSN `>= from`,
  spanning however many segments that requires - whether durably on disk
  or still sitting in the not-yet-flushed in-memory buffer. The buffer's
  bytes are exactly what `flush` would write immediately after the active
  segment's durable region, so appending them here gives the same stream
  `flush`-then-read would - which is what lets a runtime `abort` (see
  `txn::TransactionManager::abort`) find and undo a transaction's own
  records before they have ever been flushed, not just after a crash.
  Finds the latest sealed segment whose `start_lsn <= from` and starts
  there, or the earliest sealed segment at all if none qualifies - see the
  truncation-invariant section above for why "none qualifies" means "start
  from the beginning of what's retained," not "there is nothing to read" -
  then visits every later segment up to the current tail (there can be
  more than one still to visit - `from` narrows *which* segment to start
  from, but every later segment must still be scanned) under the mutex, eagerly reads only the active segment's durable
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
  segment's `start_lsn` is the largest one `<= lsn`, then either reuses
  `sealed_device_cache` if it already holds that segment's id or opens it
  through `store` and replaces the cache entry, and reads from there. A
  durable-region lookup always costs exactly two device reads (peek
  `total_len`, then read exactly that many bytes) rather than one read
  sized to the whole durable region. The one-entry cache is sized for
  `undo_transaction`'s access pattern - walking a `prev_lsn` chain strictly
  backward, so consecutive `read_at` calls that land in a sealed segment at
  all tend to land in the *same* one - not for `iter_from`, which already
  streams forward through `LogIterator` without going through `read_at`.
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
