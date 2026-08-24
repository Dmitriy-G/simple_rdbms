# wal

The write-ahead log: `LogManager`, its on-disk record format, and the
iterator recovery scans it with. Owns a database's `<db_path>.wal` file,
assigning each appended record a monotonically increasing `Lsn` and
enforcing the write-ahead rule from the durable end - a page may not be
written to the data file until the log record covering it is durable (see
`BufferPool::flush_pages`). A torn write to the WAL never needs the
double-write buffer the way a data-file page does: `append` only
serializes into an in-memory buffer, and nothing reaches disk until
`flush`, so a crash mid-append leaves, at worst, an incomplete trailing
record that `decode_record`'s own length-prefix-plus-CRC framing detects
and `open_with_device` truncates away on the next open - never a
half-written record mistaken for a valid one. This is what makes "steal"
(evicting a dirty page before its transaction commits) and "no-force" (not
flushing a transaction's pages at commit) both safe: redo replays every
logged `Update` regardless of commit status, and undo rolls back anything
that never committed - both driven by `storage::recovery`.

## Key Components

- `CHECKPOINT_TXN` - the `txn_id` every `CheckpointBegin`/`CheckpointEnd`
  record is logged under. Reserved so it can never collide with a real,
  caller-assigned transaction id (those start at `0` and count up);
  recovery's Analysis pass special-cases this id out of the Active
  Transaction Table.
- `MAGIC` - the 8-byte magic string every write-ahead log file starts
  with.
- `HEADER_LEN` - the length, in bytes, of the log file's fixed header
  (just `MAGIC`). No real record can ever start before this offset, which
  is what lets `Lsn(0)` - and, defensively, anything less than
  `HEADER_LEN` - be treated as "no record" rather than a real one: LSNs
  are byte offsets into the log stream (see `LogManager::append`), and
  offset `0` is where the header itself lives, not a record.
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
- `LogManager` - owns the log file. `device`, an in-memory `buffer` of
  records appended but not yet flushed, `next_lsn` (the byte offset the
  next `append` will assign - durable file length plus buffer length),
  `durable_lsn` (one past the last durably-written byte, never less than
  `HEADER_LEN`), `last_lsn_by_txn` (each transaction's most recently
  appended LSN, consulted by `append` to fill in `prev_lsn`), and
  `bytes_appended` (cumulative, for the checkpoint byte-threshold
  trigger).
- `LogManager::open(path)` - opens (creating if necessary) the log file
  at `path`.
- `LogManager::open_with_device(device)` - opens a log backed by an
  arbitrary `BlockDevice`, for tests and crash-injection fault wrapping. A
  brand-new (zero-length) device gets a fresh `MAGIC` header written to it
  - and so, treated identically, does one shorter than a whole header: the
  header is written exactly once, in a single `write_at` at the very
  start of a database's life, before any record could possibly have been
  appended, so a device that's nonzero but still short of `HEADER_LEN` can
  only mean that very first write was itself torn by a crash - there is
  nothing to lose by reinitializing, since nothing could have been durably
  logged yet either way. An existing device with a *complete* header
  (`len >= HEADER_LEN`) has that header validated first, then is scanned
  forward from `HEADER_LEN`, validating each record's CRC, to find the
  durable boundary and rebuild the per-transaction `prev_lsn` chain; any
  trailing bytes past the last valid record (a torn write from a crash
  mid-append) are truncated away, so appends after reopening always start
  from a clean boundary.
- `LogManager::append(&mut self, record) -> Result<Lsn, StorageError>` -
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
- `LogManager::flush(&mut self, up_to)` - forces every buffered record up
  to and including the one starting at `up_to` to disk. Idempotent:
  returns immediately if that record is already durable. The check is
  `durable_lsn > up_to.0`, strictly - not `>=`: `up_to` is a record's
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
  calls this synchronously before acknowledging a commit.
- `LogManager::flush_all(&mut self)` - forces every record appended so
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
  see a `TransactionManager`, such as `PageGuard::write`'s own `Update`
  records.
- `LogManager::max_txn_id(&self) -> Option<TxnId>` - the highest
  transaction id that appears anywhere in the log (excluding
  `CHECKPOINT_TXN`), or `None` if the log has never recorded a real
  transaction. Built from the same full-file scan `open_with_device`
  already does to rebuild `last_lsn_by_txn`, so this reflects every id the
  log has ever used - not just the ones a bounded, checkpoint-anchored
  recovery scan happens to revisit - which is what lets a reseeded
  `TransactionManager` avoid ever reusing an id whose log chain a new
  transaction of the same id would otherwise alias.
- `LogManager::iter_from(&mut self, from) -> Result<LogIterator, StorageError>` -
  returns a forward iterator over every record with LSN `>= from`,
  whether durably on disk or still sitting in the not-yet-flushed
  in-memory buffer. The buffer's bytes are exactly what `flush` would
  write immediately after the durable region, so appending them here
  gives the same stream `flush`-then-read would - which is what lets a
  runtime `abort` (see `txn::TransactionManager::abort`) find and undo a
  transaction's own records before they have ever been flushed, not just
  after a crash. Validates each durable record's CRC as it goes and stops
  cleanly - yielding no further records, not an error - at the first
  invalid or truncated one, since a crash mid-append leaves exactly that
  shape and is not corruption. Seeks straight to `from` (clamped up to
  `HEADER_LEN` if smaller) instead of reading the whole device from
  offset `0`: only `from` and later can possibly matter. `from` must be
  `HEADER_LEN` or an LSN some earlier `append` actually returned - i.e. a
  real record boundary - since the bytes read here are decoded starting
  exactly at `from` with no scan-and-skip to realign. Queries the
  device's current length fresh rather than trusting the cached
  `durable_lsn`, so a torn tail introduced *after* this `LogManager` was
  constructed is still handled by the ordinary CRC/length-prefix
  validation, not an out-of-bounds read.
- `LogManager::read_at(&mut self, lsn) -> Result<Option<LoggedRecord>, StorageError>` -
  reads and decodes exactly one record by its LSN, without scanning any
  other part of the log: `undo_transaction` uses this to walk a
  transaction's chain backward one record at a time, which is what keeps
  undoing N records `O(N)` rather than the `O(N^2)` that
  `iter_from(lsn).next()` used to cost (each call re-reading the whole log
  just to yield its first matching record). Returns `None` for `Lsn(0)`
  (the "no record" sentinel) or a torn/invalid record. A durable-region
  lookup costs two device reads (one to peek the record's own `total_len`
  prefix, one to read exactly that many bytes) rather than one read sized
  to the whole durable region. A lookup into the not-yet-flushed buffer
  costs no device I/O at all: it's already in memory.
- `LogIterator` - forward iterator over `LoggedRecord`s in a WAL file,
  produced by `LogManager::iter_from`.

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
during Undo.
