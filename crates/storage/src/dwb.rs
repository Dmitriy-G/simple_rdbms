//! The double-write buffer: a separate `<db_path>.dwb` file that gives
//! recovery an intact copy of every page about to be written to the real
//! data file, so a page torn by a crash mid-write (a single `write_at` is
//! not atomic on real hardware) can be restored instead of merely detected.
//!
//! Deliberately a separate file, not a region of the main database file:
//! page ids are baked into every WAL record already, so nothing here may
//! renumber or otherwise shift them.

use std::fs::OpenOptions;
use std::path::PathBuf;

use common::PageId;
use common::crc::crc32;

use crate::block_device::{BlockDevice, FileDevice};
use crate::error::StorageError;
use crate::page::{self, PAGE_SIZE, Page};

/// The 8-byte magic string identifying a double-write buffer file's header
/// page (page 0 of the `.dwb` file).
const MAGIC: &[u8; 8] = b"FDBDWB01";

/// Byte offset, within the header page, of the `u32` entry count.
const ENTRY_COUNT_OFFSET: usize = MAGIC.len();
/// Byte offset, within the header page, where the batch's page ids start.
const PAGE_IDS_OFFSET: usize = ENTRY_COUNT_OFFSET + 4;

/// A separate file holding, at any moment, either nothing or one in-flight
/// batch of page images: an exact copy of every page a `BufferPool` flush
/// is about to write to the real data file, made durable *before* any of
/// those real writes happen. See `BufferPool::flush_pages` for how the
/// full six-step protocol (write batch, sync, write real pages, sync,
/// clear batch, sync) uses this, and `recovery::recover_double_write` for
/// how a leftover batch is used to repair a torn real page on reopen.
///
/// Page 0 of the file is the header: `MAGIC` (8 bytes) | `entry_count`
/// (`u32`) | `entry_count` page ids (`u32` each) | a CRC-32 (`u32`) over
/// every byte before it, zero-padded to a full page. Pages `1..=capacity`
/// are page-image slots, each an ordinary checksummed `Page` image (see
/// `page::stamp_checksum`) - a torn slot write is self-detecting the same
/// way a torn real-file write is, which is exactly what lets recovery tell
/// "the crash landed while this copy was still being written" (bad
/// checksum: skip it, the real page was never touched) apart from "this
/// copy is intact" (good checksum: it's safe to restore from).
pub struct DoubleWriteBuffer {
    device: Box<dyn BlockDevice>,
    capacity: usize,
}

impl DoubleWriteBuffer {
    /// The default number of page-image slots: how many pages a single
    /// flush batch can cover before `BufferPool::flush_all` must split it
    /// into more than one batch.
    pub const DEFAULT_CAPACITY: usize = 64;

    /// Opens (creating if necessary) the double-write buffer file at
    /// `path`, sized for `capacity` page-image slots.
    pub fn open(path: impl Into<PathBuf>, capacity: usize) -> Result<Self, StorageError> {
        let path = path.into();
        let file =
            OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&path)?;
        Self::open_with_device(Box::new(FileDevice::new(file)), capacity)
    }

    /// Opens a double-write buffer backed by an arbitrary `BlockDevice`,
    /// for tests and crash-injection fault wrapping. Only ensures the
    /// device is large enough for its header page plus `capacity` slots,
    /// growing it via `set_len` if short (the same zero-fill semantics
    /// `disk::DiskManager` already relies on) - it deliberately never reads
    /// or validates the header page's content. An all-zero header page
    /// already decodes as "bad magic, nothing to recover" in `read_batch`,
    /// the same reasoning that makes an all-zero heap page a valid empty
    /// one (see `heap::NO_NEXT_PAGE`'s doc comment) rather than requiring
    /// its own special-cased initialization here.
    pub fn open_with_device(
        mut device: Box<dyn BlockDevice>,
        capacity: usize,
    ) -> Result<Self, StorageError> {
        debug_assert!(
            PAGE_IDS_OFFSET + capacity * 4 + 4 <= PAGE_SIZE,
            "a {capacity}-entry double-write buffer header does not fit in a {PAGE_SIZE}-byte page"
        );
        let expected_len = (1 + capacity) as u64 * PAGE_SIZE as u64;
        if device.size()? < expected_len {
            device.set_len(expected_len)?;
        }
        Ok(Self { device, capacity })
    }

    /// The number of page-image slots this double-write buffer has.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    fn offset_of_slot(&self, index: usize) -> u64 {
        (1 + index) as u64 * PAGE_SIZE as u64
    }

    /// Protocol steps 1-3: writes every page in `pages` into its own slot
    /// (in order, each independently checksummed - see `page::stamp_checksum`),
    /// writes the header recording this batch's page ids, then `sync_all`s
    /// the whole file. After this returns (successfully), the batch is
    /// durable and recoverable even if the caller crashes before writing a
    /// single one of these pages to its real location.
    pub fn write_batch(&mut self, pages: &[Page]) -> Result<(), StorageError> {
        debug_assert!(
            pages.len() <= self.capacity,
            "batch of {} pages exceeds the double-write buffer's {}-slot capacity",
            pages.len(),
            self.capacity
        );
        for (index, page) in pages.iter().enumerate() {
            let mut scratch = *page.data();
            page::stamp_checksum(&mut scratch);
            self.device.write_at(self.offset_of_slot(index), &scratch)?;
        }
        let header = self.encode_header(pages.iter().map(Page::id));
        self.device.write_at(0, &header)?;
        self.device.sync_all()?;
        Ok(())
    }

    /// Protocol step 6: marks the batch retired (`entry_count = 0`) and
    /// `sync_all`s. Called once every page in the batch has been written to
    /// its real location and that write is itself durable (step 5) - after
    /// this, the slot images are stale and no longer needed.
    pub fn clear_batch(&mut self) -> Result<(), StorageError> {
        let header = self.encode_header(std::iter::empty());
        self.device.write_at(0, &header)?;
        self.device.sync_all()?;
        Ok(())
    }

    fn encode_header(&self, page_ids: impl Iterator<Item = PageId>) -> [u8; PAGE_SIZE] {
        let ids: Vec<PageId> = page_ids.collect();
        let mut buf = [0u8; PAGE_SIZE];
        buf[0..MAGIC.len()].copy_from_slice(MAGIC);
        buf[ENTRY_COUNT_OFFSET..PAGE_IDS_OFFSET].copy_from_slice(&(ids.len() as u32).to_le_bytes());
        for (i, id) in ids.iter().enumerate() {
            let at = PAGE_IDS_OFFSET + i * 4;
            buf[at..at + 4].copy_from_slice(&id.0.to_le_bytes());
        }
        let content_end = PAGE_IDS_OFFSET + ids.len() * 4;
        let crc = crc32(&buf[0..content_end]);
        buf[content_end..content_end + 4].copy_from_slice(&crc.to_le_bytes());
        buf
    }

    /// Recovery: decodes the header page, returning the batch's page ids
    /// in slot order, or `None` if there is nothing to recover - bad magic,
    /// an `entry_count` of `0` or above `capacity`, or a CRC mismatch are
    /// all treated alike, as "invalid," per the recovery protocol: none of
    /// them are errors, since a header can legitimately be caught
    /// mid-write by the very crash recovery is running to clean up after.
    pub fn read_batch(&mut self) -> Result<Option<Vec<PageId>>, StorageError> {
        let mut header = [0u8; PAGE_SIZE];
        self.device.read_at(0, &mut header)?;

        if header[0..MAGIC.len()] != *MAGIC {
            return Ok(None);
        }
        let count = read_u32(&header, ENTRY_COUNT_OFFSET) as usize;
        if count == 0 || count > self.capacity {
            return Ok(None);
        }
        let content_end = PAGE_IDS_OFFSET + count * 4;
        if content_end + 4 > PAGE_SIZE {
            return Ok(None);
        }
        let stored_crc = read_u32(&header, content_end);
        if crc32(&header[0..content_end]) != stored_crc {
            return Ok(None);
        }

        let ids = (0..count).map(|i| PageId(read_u32(&header, PAGE_IDS_OFFSET + i * 4))).collect();
        Ok(Some(ids))
    }

    /// Recovery: the raw bytes of slot `index` (0-based, matching the order
    /// `read_batch` returned page ids in), whether or not they pass their
    /// own checksum - the caller (`recovery::recover_double_write`) is the
    /// one that decides what a bad copy means.
    pub fn read_slot(&mut self, index: usize) -> Result<[u8; PAGE_SIZE], StorageError> {
        let mut buf = [0u8; PAGE_SIZE];
        self.device.read_at(self.offset_of_slot(index), &mut buf)?;
        Ok(buf)
    }
}

fn read_u32(buf: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([buf[at], buf[at + 1], buf[at + 2], buf[at + 3]])
}
