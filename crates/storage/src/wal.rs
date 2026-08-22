use std::collections::HashMap;
use std::fs::OpenOptions;
use std::path::PathBuf;

use common::crc::crc32;
use common::{Lsn, PageId, TxnId};

use crate::block_device::{BlockDevice, FileDevice};
use crate::error::StorageError;

/// The `txn_id` every `CheckpointBegin`/`CheckpointEnd` record is logged
/// under. Reserved so it can never collide with a real, caller-assigned
/// transaction id (those start at `0` and count up); recovery's Analysis
/// pass special-cases this id out of the Active Transaction Table.
pub const CHECKPOINT_TXN: TxnId = TxnId(u64::MAX);

/// The fixed-size portion of a serialized record: front `total_len` (4
/// bytes), `lsn` (8), `prev_lsn` (8), `txn_id` (8), `kind` tag (1), `crc32`
/// (4), and trailing `total_len` (4). Any candidate record shorter than
/// this cannot be real, regardless of what its `total_len` field claims.
const MIN_RECORD_LEN: usize = 4 + 8 + 8 + 8 + 1 + 4 + 4;

/// The kind-specific payload of a log record, and the physical before/after
/// images needed to redo or undo it. `Update` is the one workhorse variant:
/// a byte-range write to any page, whether that page is a heap page's slot
/// array, a catalog row, or (later) a B+tree node - the log does not need
/// to know which.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogRecordKind {
    /// Marks the start of a transaction.
    Begin,
    /// Marks a transaction as committed; its updates must survive a crash
    /// from this point on.
    Commit,
    /// Marks a transaction as aborted; its updates must be undone.
    Abort,
    /// Marks that every record belonging to a transaction (redo and undo)
    /// has been fully processed; recovery need not look at it again.
    End,
    /// A physical byte-range write to `page_id` at `offset`: `before` is
    /// the bytes that were there (for undo), `after` is what replaced them
    /// (for redo).
    Update {
        /// The page being modified.
        page_id: PageId,
        /// The byte offset within the page the write starts at.
        offset: u16,
        /// The bytes at `offset..offset+len` before the write.
        before: Vec<u8>,
        /// The bytes at `offset..offset+len` after the write.
        after: Vec<u8>,
    },
    /// Records that `page_id` was allocated, extending the file.
    AllocPage {
        /// The page allocated.
        page_id: PageId,
    },
    /// A compensation log record: written while undoing an `Update`, so
    /// that a second crash during undo does not undo the same change
    /// twice. `undo_next_lsn` points at the next record still to be undone
    /// for this transaction, skipping over the one this CLR compensates
    /// for.
    Clr {
        /// The page the compensating write applies to.
        page_id: PageId,
        /// The byte offset within the page the write starts at.
        offset: u16,
        /// The bytes to reapply at `offset` (the original `before` image
        /// of the `Update` being undone).
        after: Vec<u8>,
        /// The LSN undo should continue from after this CLR, skipping the
        /// record it compensates for. `Lsn(0)` means the chain is
        /// exhausted (mirrors `prev_lsn`'s own `None`-as-`0` encoding).
        undo_next_lsn: Lsn,
    },
    /// Marks the start of a fuzzy checkpoint. Logged under `CHECKPOINT_TXN`.
    /// Its own LSN is recorded in the page-0 header so recovery's Analysis
    /// pass knows where to start scanning.
    CheckpointBegin,
    /// Carries the Active Transaction Table and Dirty Page Table snapshots
    /// captured at (approximately) the matching `CheckpointBegin`. Logged
    /// under `CHECKPOINT_TXN`.
    CheckpointEnd {
        /// Every transaction active at checkpoint time, with its `last_lsn`.
        att: Vec<(TxnId, Lsn)>,
        /// Every dirty page at checkpoint time, with its `recovery_lsn`.
        dpt: Vec<(PageId, Lsn)>,
    },
}

impl LogRecordKind {
    fn tag(&self) -> u8 {
        match self {
            LogRecordKind::Begin => 0,
            LogRecordKind::Commit => 1,
            LogRecordKind::Abort => 2,
            LogRecordKind::End => 3,
            LogRecordKind::Update { .. } => 4,
            LogRecordKind::AllocPage { .. } => 5,
            LogRecordKind::Clr { .. } => 6,
            LogRecordKind::CheckpointBegin => 7,
            LogRecordKind::CheckpointEnd { .. } => 8,
        }
    }

    fn encode_payload(&self, buf: &mut Vec<u8>) {
        match self {
            LogRecordKind::Begin
            | LogRecordKind::Commit
            | LogRecordKind::Abort
            | LogRecordKind::End
            | LogRecordKind::CheckpointBegin => {}
            LogRecordKind::Update { page_id, offset, before, after } => {
                buf.extend_from_slice(&page_id.0.to_le_bytes());
                buf.extend_from_slice(&offset.to_le_bytes());
                buf.extend_from_slice(&(before.len() as u32).to_le_bytes());
                buf.extend_from_slice(before);
                buf.extend_from_slice(&(after.len() as u32).to_le_bytes());
                buf.extend_from_slice(after);
            }
            LogRecordKind::AllocPage { page_id } => {
                buf.extend_from_slice(&page_id.0.to_le_bytes());
            }
            LogRecordKind::Clr { page_id, offset, after, undo_next_lsn } => {
                buf.extend_from_slice(&page_id.0.to_le_bytes());
                buf.extend_from_slice(&offset.to_le_bytes());
                buf.extend_from_slice(&(after.len() as u32).to_le_bytes());
                buf.extend_from_slice(after);
                buf.extend_from_slice(&undo_next_lsn.0.to_le_bytes());
            }
            LogRecordKind::CheckpointEnd { att, dpt } => {
                buf.extend_from_slice(&(att.len() as u32).to_le_bytes());
                for (txn_id, lsn) in att {
                    buf.extend_from_slice(&txn_id.0.to_le_bytes());
                    buf.extend_from_slice(&lsn.0.to_le_bytes());
                }
                buf.extend_from_slice(&(dpt.len() as u32).to_le_bytes());
                for (page_id, lsn) in dpt {
                    buf.extend_from_slice(&page_id.0.to_le_bytes());
                    buf.extend_from_slice(&lsn.0.to_le_bytes());
                }
            }
        }
    }

    /// Decodes a payload given its `tag` byte, or `None` if the bytes are
    /// too short, too long, or the tag is unrecognized - any of which means
    /// this candidate record is not a real one (its CRC should already
    /// have been checked by the caller before this is trusted).
    fn decode_payload(tag: u8, bytes: &[u8]) -> Option<Self> {
        let mut cursor = Cursor::new(bytes);
        let kind = match tag {
            0 => LogRecordKind::Begin,
            1 => LogRecordKind::Commit,
            2 => LogRecordKind::Abort,
            3 => LogRecordKind::End,
            4 => {
                let page_id = PageId(cursor.take_u32()?);
                let offset = cursor.take_u16()?;
                let before = cursor.take_len_prefixed()?;
                let after = cursor.take_len_prefixed()?;
                LogRecordKind::Update { page_id, offset, before, after }
            }
            5 => {
                let page_id = PageId(cursor.take_u32()?);
                LogRecordKind::AllocPage { page_id }
            }
            6 => {
                let page_id = PageId(cursor.take_u32()?);
                let offset = cursor.take_u16()?;
                let after = cursor.take_len_prefixed()?;
                let undo_next_lsn = Lsn(cursor.take_u64()?);
                LogRecordKind::Clr { page_id, offset, after, undo_next_lsn }
            }
            7 => LogRecordKind::CheckpointBegin,
            8 => {
                let att_len = cursor.take_u32()? as usize;
                let mut att = Vec::with_capacity(att_len);
                for _ in 0..att_len {
                    let txn_id = TxnId(cursor.take_u64()?);
                    let lsn = Lsn(cursor.take_u64()?);
                    att.push((txn_id, lsn));
                }
                let dpt_len = cursor.take_u32()? as usize;
                let mut dpt = Vec::with_capacity(dpt_len);
                for _ in 0..dpt_len {
                    let page_id = PageId(cursor.take_u32()?);
                    let lsn = Lsn(cursor.take_u64()?);
                    dpt.push((page_id, lsn));
                }
                LogRecordKind::CheckpointEnd { att, dpt }
            }
            _ => return None,
        };
        cursor.exhausted().then_some(kind)
    }
}

/// A record ready to append: the transaction it belongs to and its kind.
/// `LogManager::append` assigns the LSN and fills in `prev_lsn` by
/// chaining from that transaction's most recently appended record, so
/// callers never need to track it themselves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogRecord {
    /// The transaction this record belongs to.
    pub txn_id: TxnId,
    /// The record's kind and payload.
    pub kind: LogRecordKind,
}

/// A record read back from the log, with the envelope fields
/// `LogManager::append` assigned it at write time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoggedRecord {
    /// This record's own log sequence number.
    pub lsn: Lsn,
    /// The LSN of this transaction's previous record, or `None` if this is
    /// its first.
    pub prev_lsn: Option<Lsn>,
    /// The transaction this record belongs to.
    pub txn_id: TxnId,
    /// The record's kind and payload.
    pub kind: LogRecordKind,
}

/// A minimal, panic-free byte reader: every `take*` call either returns the
/// requested field or `None`, never panics on a short or malformed buffer.
struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let slice = self.bytes.get(self.pos..self.pos + n)?;
        self.pos += n;
        Some(slice)
    }

    fn take_u16(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes(self.take(2)?.try_into().ok()?))
    }

    fn take_u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    fn take_u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }

    /// Reads a `u32` length prefix followed by that many bytes.
    fn take_len_prefixed(&mut self) -> Option<Vec<u8>> {
        let len = self.take_u32()? as usize;
        Some(self.take(len)?.to_vec())
    }

    /// `true` if every byte has been consumed - a malformed payload (extra
    /// trailing bytes the tag's decoder didn't account for) fails this.
    fn exhausted(&self) -> bool {
        self.pos == self.bytes.len()
    }
}

/// Serializes `lsn`/`prev_lsn`/`txn_id`/`kind` into a complete, self-
/// describing record: `total_len | lsn | prev_lsn | txn_id | kind |
/// payload | crc32 | total_len`. `prev_lsn` of `None` is encoded as `0`,
/// which is safe because LSNs are assigned starting at `1`.
fn encode_record(lsn: Lsn, prev_lsn: Option<Lsn>, txn_id: TxnId, kind: &LogRecordKind) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&lsn.0.to_le_bytes());
    body.extend_from_slice(&prev_lsn.map_or(0, |lsn| lsn.0).to_le_bytes());
    body.extend_from_slice(&txn_id.0.to_le_bytes());
    body.push(kind.tag());
    kind.encode_payload(&mut body);

    let total_len = (4 + body.len() + 4 + 4) as u32;

    let mut record = Vec::with_capacity(total_len as usize);
    record.extend_from_slice(&total_len.to_le_bytes());
    record.extend_from_slice(&body);
    let crc = crc32(&record);
    record.extend_from_slice(&crc.to_le_bytes());
    record.extend_from_slice(&total_len.to_le_bytes());
    record
}

/// Decodes the record starting at `bytes[pos..]`, returning it along with
/// the number of bytes it occupies, or `None` if there is no complete,
/// CRC-valid record there - either because fewer than `total_len` bytes
/// remain (a torn write from a crash mid-append) or because the CRC does
/// not match (a flipped or corrupted byte). Both cases are reported
/// identically: recovery treats a crash mid-append as normal, not as
/// corruption to raise an error over.
fn decode_record(bytes: &[u8], pos: usize) -> Option<(LoggedRecord, usize)> {
    let front_len_bytes = bytes.get(pos..pos + 4)?;
    let total_len = u32::from_le_bytes(front_len_bytes.try_into().ok()?) as usize;
    if total_len < MIN_RECORD_LEN {
        return None;
    }
    let record = bytes.get(pos..pos + total_len)?;

    let trailing_len = &record[total_len - 4..];
    if trailing_len != &record[0..4] {
        return None;
    }
    let stored_crc = u32::from_le_bytes(record[total_len - 8..total_len - 4].try_into().ok()?);
    if crc32(&record[0..total_len - 8]) != stored_crc {
        return None;
    }

    let mut cursor = Cursor::new(&record[4..total_len - 8]);
    let lsn = Lsn(cursor.take_u64()?);
    let prev_lsn_raw = cursor.take_u64()?;
    let prev_lsn = (prev_lsn_raw != 0).then_some(Lsn(prev_lsn_raw));
    let txn_id = TxnId(cursor.take_u64()?);
    let tag = *cursor.take(1)?.first()?;
    let payload = cursor.take(cursor.bytes.len() - cursor.pos)?;
    let kind = LogRecordKind::decode_payload(tag, payload)?;

    Some((LoggedRecord { lsn, prev_lsn, txn_id, kind }, total_len))
}

/// Owns a database's write-ahead log file (`<db_path>.wal`), assigning
/// each appended record a monotonically increasing `Lsn` and enforcing the
/// write-ahead rule from the durable end: a page may not be written to the
/// data file until the log record covering it is durable (see
/// `BufferPool::flush_frame`).
///
/// `append` only serializes into an in-memory buffer; nothing reaches disk
/// until `flush`. This is what makes "steal" (evicting a dirty page before
/// its transaction commits) and "no-force" (not flushing a transaction's
/// pages at commit) both safe: redo replays every logged `Update`
/// regardless of commit status, and undo rolls back anything that never
/// committed - both driven by `storage::recovery`.
pub struct LogManager {
    device: Box<dyn BlockDevice>,
    /// Records appended but not yet flushed to disk.
    buffer: Vec<u8>,
    /// The LSN the next `append` will assign.
    next_lsn: u64,
    /// The highest LSN durably on disk; `0` if nothing has been flushed
    /// yet (LSNs are assigned starting at `1`).
    durable_lsn: u64,
    /// Each transaction's most recently appended LSN, consulted by
    /// `append` to fill in a new record's `prev_lsn`.
    last_lsn_by_txn: HashMap<TxnId, u64>,
    /// Cumulative bytes appended since this `LogManager` was opened, for
    /// the checkpoint byte-threshold trigger. Not reset by a checkpoint;
    /// callers compare against the value observed at the last checkpoint.
    bytes_appended: u64,
}

impl LogManager {
    /// Opens (creating if necessary) the log file at `path`.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, StorageError> {
        let path = path.into();
        let file =
            OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&path)?;
        Self::open_with_device(Box::new(FileDevice::new(file)))
    }

    /// Opens a log backed by an arbitrary `BlockDevice`, for tests and
    /// crash-injection fault wrapping. An existing device is scanned
    /// forward, validating each record's CRC, to find `next_lsn` and
    /// rebuild the per-transaction `prev_lsn` chain; any trailing bytes
    /// past the last valid record (a torn write from a crash mid-append)
    /// are truncated away, so appends after reopening always start from a
    /// clean boundary.
    pub fn open_with_device(mut device: Box<dyn BlockDevice>) -> Result<Self, StorageError> {
        let len = device.size()?;
        let mut bytes = vec![0u8; len as usize];
        device.read_at(0, &mut bytes)?;

        let mut pos = 0usize;
        let mut last_lsn = 0u64;
        let mut last_lsn_by_txn = HashMap::new();
        while let Some((record, len)) = decode_record(&bytes, pos) {
            last_lsn = record.lsn.0;
            last_lsn_by_txn.insert(record.txn_id, record.lsn.0);
            pos += len;
        }
        device.set_len(pos as u64)?;

        Ok(Self {
            device,
            buffer: Vec::new(),
            next_lsn: last_lsn + 1,
            durable_lsn: last_lsn,
            last_lsn_by_txn,
            bytes_appended: 0,
        })
    }

    /// Appends `record` to the log, returning its assigned `Lsn`. Only
    /// buffers the bytes in memory; call `flush` to force them to disk.
    pub fn append(&mut self, record: LogRecord) -> Result<Lsn, StorageError> {
        let lsn = Lsn(self.next_lsn);
        self.next_lsn += 1;
        let prev_lsn = self.last_lsn_by_txn.get(&record.txn_id).copied().map(Lsn);
        let bytes = encode_record(lsn, prev_lsn, record.txn_id, &record.kind);
        self.bytes_appended += bytes.len() as u64;
        self.buffer.extend_from_slice(&bytes);
        self.last_lsn_by_txn.insert(record.txn_id, lsn.0);
        Ok(lsn)
    }

    /// Forces every buffered record up to and including `up_to` to disk.
    /// Idempotent: returns immediately if `up_to` is already durable.
    pub fn flush(&mut self, up_to: Lsn) -> Result<(), StorageError> {
        if self.durable_lsn >= up_to.0 {
            return Ok(());
        }
        let offset = self.device.size()?;
        self.device.write_at(offset, &self.buffer)?;
        self.device.sync_all()?;
        self.buffer.clear();
        self.durable_lsn = self.next_lsn - 1;
        Ok(())
    }

    /// Forces every record appended so far to disk, regardless of what any
    /// individual page needs. Used at clean shutdown.
    pub fn flush_all(&mut self) -> Result<(), StorageError> {
        if self.next_lsn <= 1 {
            return Ok(());
        }
        self.flush(Lsn(self.next_lsn - 1))
    }

    /// The highest LSN durably on disk, or `Lsn(0)` if nothing has been
    /// flushed yet.
    pub fn durable_lsn(&self) -> Lsn {
        Lsn(self.durable_lsn)
    }

    /// Cumulative bytes appended since this `LogManager` was opened.
    pub fn bytes_appended(&self) -> u64 {
        self.bytes_appended
    }

    /// `txn_id`'s most recently appended LSN, or `None` if it has never
    /// appended a record. This is the authoritative source for a
    /// transaction's tail of the log - unlike a value cached at `begin`
    /// time, it reflects every record appended since, including ones
    /// appended through paths that never see a `TransactionManager`, such
    /// as `PageGuard::write`'s own `Update` records.
    pub fn last_lsn_for(&self, txn_id: TxnId) -> Option<Lsn> {
        self.last_lsn_by_txn.get(&txn_id).copied().map(Lsn)
    }

    /// Returns a forward iterator over every record with LSN `>= from`,
    /// whether durably on disk or still sitting in the not-yet-flushed
    /// in-memory buffer. The buffer's bytes are exactly what `flush` would
    /// write immediately after the durable region, so appending them here
    /// gives the same stream `flush`-then-read would - which is what lets
    /// a runtime `abort` (see `txn::TransactionManager::abort`) find and
    /// undo a transaction's own records before they have ever been
    /// flushed, not just after a crash (where nothing is left unflushed
    /// anyway, since a fresh process starts with an empty buffer).
    /// Validates each durable record's CRC as it goes and stops cleanly -
    /// yielding no further records, not an error - at the first invalid or
    /// truncated one, since a crash mid-append leaves exactly that shape
    /// and is not corruption.
    pub fn iter_from(&mut self, from: Lsn) -> Result<LogIterator, StorageError> {
        let len = self.device.size()?;
        let mut bytes = vec![0u8; len as usize];
        self.device.read_at(0, &mut bytes)?;
        bytes.extend_from_slice(&self.buffer);
        Ok(LogIterator { bytes, pos: 0, from })
    }
}

/// Forward iterator over `LoggedRecord`s in a WAL file, produced by
/// `LogManager::iter_from`.
pub struct LogIterator {
    bytes: Vec<u8>,
    pos: usize,
    from: Lsn,
}

impl Iterator for LogIterator {
    type Item = LoggedRecord;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let (record, len) = decode_record(&self.bytes, self.pos)?;
            self.pos += len;
            if record.lsn >= self.from {
                return Some(record);
            }
        }
    }
}
