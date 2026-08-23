use std::fs::OpenOptions;
use std::path::PathBuf;

use common::PageId;
use common::crc::crc32;

use crate::block_device::{BlockDevice, FileDevice};
use crate::error::StorageError;
use crate::page::{self, PAGE_SIZE, Page};

const MAGIC: &[u8; 8] = b"FDBDWB01";

const ENTRY_COUNT_OFFSET: usize = MAGIC.len();
const PAGE_IDS_OFFSET: usize = ENTRY_COUNT_OFFSET + 4;

pub struct DoubleWriteBuffer {
    device: Box<dyn BlockDevice>,
    capacity: usize,
}

impl DoubleWriteBuffer {
    pub const DEFAULT_CAPACITY: usize = 64;

    pub fn open(path: impl Into<PathBuf>, capacity: usize) -> Result<Self, StorageError> {
        let path = path.into();
        let file =
            OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&path)?;
        Self::open_with_device(Box::new(FileDevice::new(file)), capacity)
    }

    pub fn open_with_device(
        mut device: Box<dyn BlockDevice>,
        capacity: usize,
    ) -> Result<Self, StorageError> {
        if capacity == 0 {
            return Err(StorageError::InvalidDwbCapacity);
        }
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

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    fn offset_of_slot(&self, index: usize) -> u64 {
        (1 + index) as u64 * PAGE_SIZE as u64
    }

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

    pub fn read_slot(&mut self, index: usize) -> Result<[u8; PAGE_SIZE], StorageError> {
        let mut buf = [0u8; PAGE_SIZE];
        self.device.read_at(self.offset_of_slot(index), &mut buf)?;
        Ok(buf)
    }
}

fn read_u32(buf: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([buf[at], buf[at + 1], buf[at + 2], buf[at + 3]])
}
