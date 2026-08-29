use std::collections::{HashMap, VecDeque};
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use common::crc::crc32;
use common::sync::recover_lock;
use common::{Lsn, PageId, TxnId};

use crate::block_device::{BlockDevice, FileDevice};
use crate::error::StorageError;

pub const CHECKPOINT_TXN: TxnId = TxnId(u64::MAX);

const SEGMENT_MAGIC: &[u8; 8] = b"FDBWAL01";

const SEGMENT_HEADER_LEN: u64 = 16;

pub const HEADER_LEN: u64 = SEGMENT_HEADER_LEN;

pub const DEFAULT_SEGMENT_SIZE: u64 = 16 * 1024 * 1024;

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

pub trait SegmentStore: Send + Sync {
    fn existing_segments(&self) -> Result<Vec<u64>, StorageError>;

    fn open(&self, id: u64) -> Result<Box<dyn BlockDevice>, StorageError>;

    fn remove(&self, id: u64) -> Result<(), StorageError>;
}

pub fn segment_path(base: &Path, id: u64) -> PathBuf {
    let mut name = base.as_os_str().to_owned();
    name.push(format!(".{id:06}"));
    PathBuf::from(name)
}

pub struct FileSegmentStore {
    base: PathBuf,
}

impl FileSegmentStore {
    pub fn new(base: impl Into<PathBuf>) -> Self {
        Self { base: base.into() }
    }
}

impl SegmentStore for FileSegmentStore {
    fn existing_segments(&self) -> Result<Vec<u64>, StorageError> {
        let dir = self.base.parent().filter(|p| !p.as_os_str().is_empty());
        let dir = dir.unwrap_or_else(|| Path::new("."));
        let Some(file_name) = self.base.file_name().and_then(|n| n.to_str()) else {
            return Ok(Vec::new());
        };
        let prefix = format!("{file_name}.");

        let mut ids = Vec::new();
        match std::fs::read_dir(dir) {
            Ok(entries) => {
                for entry in entries {
                    let entry = entry?;
                    let name = entry.file_name();
                    let Some(name) = name.to_str() else { continue };
                    let Some(suffix) = name.strip_prefix(&prefix) else { continue };
                    if suffix.len() == 6
                        && !suffix.is_empty()
                        && suffix.bytes().all(|b| b.is_ascii_digit())
                        && let Ok(id) = suffix.parse::<u64>()
                    {
                        ids.push(id);
                    }
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(err.into()),
        }
        ids.sort_unstable();
        Ok(ids)
    }

    fn open(&self, id: u64) -> Result<Box<dyn BlockDevice>, StorageError> {
        let path = segment_path(&self.base, id);
        let file =
            OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&path)?;
        Ok(Box::new(FileDevice::new(file)))
    }

    fn remove(&self, id: u64) -> Result<(), StorageError> {
        let path = segment_path(&self.base, id);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err.into()),
        }
    }
}

struct NoSegmentStore;

impl SegmentStore for NoSegmentStore {
    fn existing_segments(&self) -> Result<Vec<u64>, StorageError> {
        Ok(Vec::new())
    }

    fn open(&self, _id: u64) -> Result<Box<dyn BlockDevice>, StorageError> {
        Err(StorageError::CorruptLogHeader {
            reason: "a log opened from a single device has no segment store to roll to".to_string(),
        })
    }

    fn remove(&self, _id: u64) -> Result<(), StorageError> {
        Ok(())
    }
}

fn write_segment_header(device: &dyn BlockDevice, start_lsn: u64) -> Result<(), StorageError> {
    let mut buf = [0u8; SEGMENT_HEADER_LEN as usize];
    buf[0..8].copy_from_slice(SEGMENT_MAGIC);
    buf[8..16].copy_from_slice(&start_lsn.to_le_bytes());
    device.set_len(0)?;
    device.write_at(0, &buf)?;
    Ok(())
}

fn read_segment_header(device: &dyn BlockDevice) -> Result<u64, StorageError> {
    let mut buf = [0u8; SEGMENT_HEADER_LEN as usize];
    device.read_at(0, &mut buf)?;
    if buf[0..8] != SEGMENT_MAGIC[..] {
        return Err(StorageError::CorruptLogHeader {
            reason: "bad magic in write-ahead log segment header".to_string(),
        });
    }
    Ok(u64::from_le_bytes([buf[8], buf[9], buf[10], buf[11], buf[12], buf[13], buf[14], buf[15]]))
}

fn scan_segment(
    device: &dyn BlockDevice,
    start_lsn: u64,
) -> Result<(u64, HashMap<TxnId, u64>), StorageError> {
    let device_len = device.size()?;
    let mut bytes = vec![0u8; device_len as usize];
    device.read_at(0, &mut bytes)?;

    let mut pos = SEGMENT_HEADER_LEN as usize;
    let mut last_lsn_by_txn = HashMap::new();
    while let Some((record, len)) = decode_record(&bytes, pos) {
        last_lsn_by_txn.insert(record.txn_id, record.lsn.0);
        pos += len;
    }
    device.set_len(pos as u64)?;

    let next_lsn = start_lsn + (pos as u64 - SEGMENT_HEADER_LEN);
    Ok((next_lsn, last_lsn_by_txn))
}

fn read_record_at(
    device: &dyn BlockDevice,
    offset: u64,
) -> Result<Option<LoggedRecord>, StorageError> {
    let device_len = device.size()?;
    if offset + 4 > device_len {
        return Ok(None);
    }
    let mut len_buf = [0u8; 4];
    device.read_at(offset, &mut len_buf)?;
    let total_len = u32::from_le_bytes(len_buf) as u64;
    if (total_len as usize) < MIN_RECORD_LEN || offset + total_len > device_len {
        return Ok(None);
    }
    let mut record_buf = vec![0u8; total_len as usize];
    device.read_at(offset, &mut record_buf)?;
    Ok(decode_record(&record_buf, 0).map(|(record, _)| record))
}

struct SegmentMeta {
    id: u64,
    start_lsn: u64,
}

struct LogBufferInner {
    store: Arc<dyn SegmentStore>,
    target_segment_size: u64,
    sealed: Vec<SegmentMeta>,
    active_id: u64,
    active_start_lsn: u64,
    active_device: Box<dyn BlockDevice>,
    buffer: Vec<u8>,
    next_lsn: u64,
    last_lsn_by_txn: HashMap<TxnId, u64>,
    bytes_appended: u64,
}

impl LogBufferInner {
    fn roll_segment(&mut self) -> Result<(), StorageError> {
        self.sealed.push(SegmentMeta { id: self.active_id, start_lsn: self.active_start_lsn });
        let new_id = self.active_id + 1;
        let new_start_lsn = self.next_lsn;
        let device = self.store.open(new_id)?;
        write_segment_header(device.as_ref(), new_start_lsn)?;
        self.active_device = device;
        self.active_id = new_id;
        self.active_start_lsn = new_start_lsn;
        Ok(())
    }
}

pub struct LogManager {
    inner: Mutex<LogBufferInner>,
    durable_lsn: AtomicU64,
}

impl LogManager {
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, StorageError> {
        let store: Arc<dyn SegmentStore> = Arc::new(FileSegmentStore::new(path.into()));
        Self::open_with_store(store, DEFAULT_SEGMENT_SIZE)
    }

    #[cfg(any(test, feature = "test-util"))]
    pub fn open_with_segment_size(
        path: impl Into<PathBuf>,
        target_segment_size: u64,
    ) -> Result<Self, StorageError> {
        let store: Arc<dyn SegmentStore> = Arc::new(FileSegmentStore::new(path.into()));
        Self::open_with_store(store, target_segment_size)
    }

    pub fn open_with_device(device: Box<dyn BlockDevice>) -> Result<Self, StorageError> {
        let store: Arc<dyn SegmentStore> = Arc::new(NoSegmentStore);
        Self::bootstrap(store, u64::MAX, 0, device, Vec::new())
    }

    #[cfg(any(test, feature = "test-util"))]
    pub fn open_with_segment_store(
        store: Arc<dyn SegmentStore>,
        target_segment_size: u64,
    ) -> Result<Self, StorageError> {
        Self::open_with_store(store, target_segment_size)
    }

    #[cfg(any(test, feature = "test-util"))]
    pub fn segment_ids(&self) -> Vec<u64> {
        let inner = recover_lock(self.inner.lock(), "LogManager.inner");
        let mut ids: Vec<u64> = inner.sealed.iter().map(|s| s.id).collect();
        ids.push(inner.active_id);
        ids
    }

    fn open_with_store(
        store: Arc<dyn SegmentStore>,
        target_segment_size: u64,
    ) -> Result<Self, StorageError> {
        let ids = store.existing_segments()?;
        let (active_id, active_device, sealed_ids) = match ids.split_last() {
            Some((&active_id, sealed_ids)) => {
                (active_id, store.open(active_id)?, sealed_ids.to_vec())
            }
            None => (0, store.open(0)?, Vec::new()),
        };
        Self::bootstrap(store, target_segment_size, active_id, active_device, sealed_ids)
    }

    fn bootstrap(
        store: Arc<dyn SegmentStore>,
        target_segment_size: u64,
        active_id: u64,
        active_device: Box<dyn BlockDevice>,
        sealed_ids: Vec<u64>,
    ) -> Result<Self, StorageError> {
        let mut sealed = Vec::with_capacity(sealed_ids.len());
        let mut last_lsn_by_txn = HashMap::new();
        for id in sealed_ids {
            let device = store.open(id)?;
            let start_lsn = read_segment_header(device.as_ref())?;
            let (_, contributions) = scan_segment(device.as_ref(), start_lsn)?;
            last_lsn_by_txn.extend(contributions);
            sealed.push(SegmentMeta { id, start_lsn });
        }

        let active_device_len = active_device.size()?;
        let (active_start_lsn, next_lsn) = if active_device_len < SEGMENT_HEADER_LEN {
            write_segment_header(active_device.as_ref(), HEADER_LEN)?;
            (HEADER_LEN, HEADER_LEN)
        } else {
            let start_lsn = read_segment_header(active_device.as_ref())?;
            let (next_lsn, contributions) = scan_segment(active_device.as_ref(), start_lsn)?;
            last_lsn_by_txn.extend(contributions);
            (start_lsn, next_lsn)
        };

        Ok(Self {
            inner: Mutex::new(LogBufferInner {
                store,
                target_segment_size,
                sealed,
                active_id,
                active_start_lsn,
                active_device,
                buffer: Vec::new(),
                next_lsn,
                last_lsn_by_txn,
                bytes_appended: 0,
            }),
            durable_lsn: AtomicU64::new(next_lsn),
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
        let offset = inner.active_device.size()?;
        inner.active_device.write_at(offset, &inner.buffer)?;
        let fsync_start = std::time::Instant::now();
        inner.active_device.sync_all()?;
        metrics::counter!("wal_fsync_total").increment(1);
        metrics::histogram!("wal_fsync_duration_seconds")
            .record(fsync_start.elapsed().as_secs_f64());
        inner.buffer.clear();
        self.durable_lsn.store(inner.next_lsn, Ordering::Release);

        let active_len = inner.active_device.size()?;
        if active_len.saturating_sub(SEGMENT_HEADER_LEN) >= inner.target_segment_size {
            inner.roll_segment()?;
        }
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

    pub fn truncate_below(&self, bound: Lsn) -> Result<(), StorageError> {
        let (store, to_remove) = {
            let mut inner = recover_lock(self.inner.lock(), "LogManager.inner");
            let mut keep_from = 0;
            for i in 0..inner.sealed.len() {
                let segment_end =
                    inner.sealed.get(i + 1).map_or(inner.active_start_lsn, |s| s.start_lsn);
                if segment_end <= bound.0 {
                    keep_from = i + 1;
                } else {
                    break;
                }
            }
            if keep_from == 0 {
                return Ok(());
            }
            let removed: Vec<u64> = inner.sealed.drain(..keep_from).map(|s| s.id).collect();
            (inner.store.clone(), removed)
        };
        for id in to_remove {
            store.remove(id)?;
        }
        Ok(())
    }

    pub fn iter_from(&self, from: Lsn) -> Result<LogIterator, StorageError> {
        let from = Lsn(from.0.max(HEADER_LEN));
        let inner = recover_lock(self.inner.lock(), "LogManager.inner");

        let start_index = inner.sealed.iter().rposition(|s| s.start_lsn <= from.0);
        let pending_segments: VecDeque<u64> = match start_index {
            Some(i) => inner.sealed[i..].iter().map(|s| s.id).collect(),
            None => VecDeque::new(),
        };

        let active_len = inner.active_device.size()?;
        let durable_part = (active_len - SEGMENT_HEADER_LEN) as usize;
        let mut tail = vec![0u8; durable_part + inner.buffer.len()];
        inner.active_device.read_at(SEGMENT_HEADER_LEN, &mut tail[..durable_part])?;
        tail[durable_part..].copy_from_slice(&inner.buffer);

        Ok(LogIterator {
            store: inner.store.clone(),
            pending_segments,
            tail: Some(tail),
            current: Vec::new(),
            pos: 0,
            from,
        })
    }

    pub fn read_at(&self, lsn: Lsn) -> Result<Option<LoggedRecord>, StorageError> {
        let offset = lsn.0;
        if offset == 0 {
            return Ok(None);
        }
        let inner = recover_lock(self.inner.lock(), "LogManager.inner");
        let durable_lsn = self.durable_lsn.load(Ordering::Acquire);
        if offset >= durable_lsn {
            let buf_offset = (offset - durable_lsn) as usize;
            return Ok(decode_record(&inner.buffer, buf_offset).map(|(record, _)| record));
        }
        if offset >= inner.active_start_lsn {
            let local = SEGMENT_HEADER_LEN + (offset - inner.active_start_lsn);
            return read_record_at(inner.active_device.as_ref(), local);
        }
        let Some(meta) = inner.sealed.iter().rev().find(|s| s.start_lsn <= offset) else {
            return Ok(None);
        };
        let local = SEGMENT_HEADER_LEN + (offset - meta.start_lsn);
        let device = inner.store.open(meta.id)?;
        read_record_at(device.as_ref(), local)
    }
}

pub struct LogIterator {
    store: Arc<dyn SegmentStore>,
    pending_segments: VecDeque<u64>,
    tail: Option<Vec<u8>>,
    current: Vec<u8>,
    pos: usize,
    from: Lsn,
}

impl Iterator for LogIterator {
    type Item = LoggedRecord;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some((record, len)) = decode_record(&self.current, self.pos) {
                self.pos += len;
                if record.lsn >= self.from {
                    return Some(record);
                }
                continue;
            }
            if let Some(id) = self.pending_segments.pop_front() {
                let device = self.store.open(id).ok()?;
                let len = device.size().ok()?;
                let mut buf = vec![0u8; len as usize];
                device.read_at(0, &mut buf).ok()?;
                self.current = buf;
                self.pos = SEGMENT_HEADER_LEN as usize;
            } else {
                self.current = self.tail.take()?;
                self.pos = 0;
            }
        }
    }
}
