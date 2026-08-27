use std::collections::HashMap;
use std::fs::OpenOptions;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use common::crc::crc32;
use common::sync::recover_lock;
use common::{Lsn, PageId, TxnId};

use crate::block_device::{BlockDevice, FileDevice};
use crate::error::StorageError;

pub const CHECKPOINT_TXN: TxnId = TxnId(u64::MAX);

const MAGIC: &[u8; 8] = b"FDBWAL01";

pub const HEADER_LEN: u64 = MAGIC.len() as u64;

const MIN_RECORD_LEN: usize = 4 + 8 + 8 + 8 + 1 + 4 + 4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogRecordKind {
    Begin,
    Commit,
    Abort,
    End,
    Update { page_id: PageId, offset: u16, before: Vec<u8>, after: Vec<u8> },
    AllocPage { page_id: PageId },
    Clr { page_id: PageId, offset: u16, after: Vec<u8>, undo_next_lsn: Lsn },
    CheckpointBegin,
    CheckpointEnd { att: Vec<(TxnId, Lsn)>, dpt: Vec<(PageId, Lsn)> },
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogRecord {
    pub txn_id: TxnId,
    pub kind: LogRecordKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoggedRecord {
    pub lsn: Lsn,
    pub prev_lsn: Option<Lsn>,
    pub txn_id: TxnId,
    pub kind: LogRecordKind,
}

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

    fn take_len_prefixed(&mut self) -> Option<Vec<u8>> {
        let len = self.take_u32()? as usize;
        Some(self.take(len)?.to_vec())
    }

    fn exhausted(&self) -> bool {
        self.pos == self.bytes.len()
    }
}

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

struct LogBufferInner {
    device: Box<dyn BlockDevice>,
    buffer: Vec<u8>,
    next_lsn: u64,
    last_lsn_by_txn: HashMap<TxnId, u64>,
    bytes_appended: u64,
}

pub struct LogManager {
    inner: Mutex<LogBufferInner>,
    durable_lsn: AtomicU64,
}

impl LogManager {
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, StorageError> {
        let path = path.into();
        let file =
            OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&path)?;
        Self::open_with_device(Box::new(FileDevice::new(file)))
    }

    pub fn open_with_device(device: Box<dyn BlockDevice>) -> Result<Self, StorageError> {
        let device_len = device.size()?;

        if device_len < HEADER_LEN {
            device.set_len(0)?;
            device.write_at(0, MAGIC)?;
            return Ok(Self {
                inner: Mutex::new(LogBufferInner {
                    device,
                    buffer: Vec::new(),
                    next_lsn: HEADER_LEN,
                    last_lsn_by_txn: HashMap::new(),
                    bytes_appended: 0,
                }),
                durable_lsn: AtomicU64::new(HEADER_LEN),
            });
        }

        let mut bytes = vec![0u8; device_len as usize];
        device.read_at(0, &mut bytes)?;

        if &bytes[0..HEADER_LEN as usize] != MAGIC.as_slice() {
            return Err(StorageError::CorruptLogHeader {
                reason: "bad magic in write-ahead log header".to_string(),
            });
        }

        let mut pos = HEADER_LEN as usize;
        let mut last_lsn_by_txn = HashMap::new();
        while let Some((record, len)) = decode_record(&bytes, pos) {
            last_lsn_by_txn.insert(record.txn_id, record.lsn.0);
            pos += len;
        }
        device.set_len(pos as u64)?;

        let boundary = pos as u64;
        Ok(Self {
            inner: Mutex::new(LogBufferInner {
                device,
                buffer: Vec::new(),
                next_lsn: boundary,
                last_lsn_by_txn,
                bytes_appended: 0,
            }),
            durable_lsn: AtomicU64::new(boundary),
        })
    }

    pub fn append(&self, record: LogRecord) -> Result<Lsn, StorageError> {
        let mut inner = recover_lock(self.inner.lock(), "LogManager.inner");
        let lsn = Lsn(inner.next_lsn);
        let prev_lsn = inner.last_lsn_by_txn.get(&record.txn_id).copied().map(Lsn);
        let bytes = encode_record(lsn, prev_lsn, record.txn_id, &record.kind);
        inner.next_lsn += bytes.len() as u64;
        inner.bytes_appended += bytes.len() as u64;
        inner.buffer.extend_from_slice(&bytes);
        inner.last_lsn_by_txn.insert(record.txn_id, lsn.0);
        tracing::trace!(lsn = lsn.0, txn_id = record.txn_id.0, "append");
        metrics::counter!("wal_bytes_written_total").increment(bytes.len() as u64);
        Ok(lsn)
    }

    pub fn flush(&self, up_to: Lsn) -> Result<(), StorageError> {
        if self.durable_lsn.load(Ordering::Acquire) > up_to.0 {
            return Ok(());
        }
        let mut inner = recover_lock(self.inner.lock(), "LogManager.inner");
        let offset = inner.device.size()?;
        inner.device.write_at(offset, &inner.buffer)?;
        let fsync_start = std::time::Instant::now();
        inner.device.sync_all()?;
        metrics::counter!("wal_fsync_total").increment(1);
        metrics::histogram!("wal_fsync_duration_seconds")
            .record(fsync_start.elapsed().as_secs_f64());
        inner.buffer.clear();
        self.durable_lsn.store(inner.next_lsn, Ordering::Release);
        Ok(())
    }

    pub fn flush_all(&self) -> Result<(), StorageError> {
        let next_lsn = {
            let inner = recover_lock(self.inner.lock(), "LogManager.inner");
            if inner.buffer.is_empty() {
                return Ok(());
            }
            inner.next_lsn
        };
        self.flush(Lsn(next_lsn))
    }

    pub fn durable_lsn(&self) -> Lsn {
        Lsn(self.durable_lsn.load(Ordering::Acquire))
    }

    pub fn bytes_appended(&self) -> u64 {
        recover_lock(self.inner.lock(), "LogManager.inner").bytes_appended
    }

    pub fn last_lsn_for(&self, txn_id: TxnId) -> Option<Lsn> {
        recover_lock(self.inner.lock(), "LogManager.inner")
            .last_lsn_by_txn
            .get(&txn_id)
            .copied()
            .map(Lsn)
    }

    pub fn max_txn_id(&self) -> Option<TxnId> {
        recover_lock(self.inner.lock(), "LogManager.inner")
            .last_lsn_by_txn
            .keys()
            .filter(|&&id| id != CHECKPOINT_TXN)
            .max()
            .copied()
    }

    pub fn iter_from(&self, from: Lsn) -> Result<LogIterator, StorageError> {
        let start = from.0.max(HEADER_LEN);
        let inner = recover_lock(self.inner.lock(), "LogManager.inner");
        let device_len = inner.device.size()?;
        let bytes = if start < device_len {
            let mut buf = vec![0u8; (device_len - start) as usize];
            inner.device.read_at(start, &mut buf)?;
            buf.extend_from_slice(&inner.buffer);
            buf
        } else {
            let buf_offset = (start - device_len) as usize;
            inner.buffer.get(buf_offset..).unwrap_or(&[]).to_vec()
        };
        Ok(LogIterator { bytes, pos: 0, from: Lsn(start) })
    }

    pub fn read_at(&self, lsn: Lsn) -> Result<Option<LoggedRecord>, StorageError> {
        let offset = lsn.0;
        if offset == 0 {
            return Ok(None);
        }
        let inner = recover_lock(self.inner.lock(), "LogManager.inner");
        let durable_lsn = self.durable_lsn.load(Ordering::Acquire);
        if offset < durable_lsn {
            let mut len_buf = [0u8; 4];
            inner.device.read_at(offset, &mut len_buf)?;
            let total_len = u32::from_le_bytes(len_buf) as u64;
            if total_len < MIN_RECORD_LEN as u64 || offset + total_len > durable_lsn {
                return Ok(None);
            }
            let mut record_buf = vec![0u8; total_len as usize];
            inner.device.read_at(offset, &mut record_buf)?;
            Ok(decode_record(&record_buf, 0).map(|(record, _)| record))
        } else {
            let buf_offset = (offset - durable_lsn) as usize;
            Ok(decode_record(&inner.buffer, buf_offset).map(|(record, _)| record))
        }
    }
}

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
