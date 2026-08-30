use std::fs::OpenOptions;
use std::path::PathBuf;
use std::sync::Mutex;

use common::PageId;
use common::crc::crc32;
use common::sync::recover_lock;

use crate::block_device::{BlockDevice, FileDevice};
use crate::error::StorageError;
use crate::page::{self, PAGE_SIZE, Page};

const MAGIC: &[u8; 8] = b"FDBDWB02";

const ENTRY_COUNT_OFFSET: usize = MAGIC.len();
const ENTRIES_OFFSET: usize = ENTRY_COUNT_OFFSET + 4;
const ENTRY_SIZE: usize = 8;

pub struct DoubleWriteBuffer {
    device: Mutex<Box<dyn BlockDevice>>,
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
        device: Box<dyn BlockDevice>,
        capacity: usize,
    ) -> Result<Self, StorageError> {
        if capacity == 0 {
            return Err(StorageError::InvalidDwbCapacity);
        }
        debug_assert!(
            ENTRIES_OFFSET + capacity * ENTRY_SIZE + 4 <= PAGE_SIZE,
            "a {capacity}-entry double-write buffer header does not fit in a {PAGE_SIZE}-byte page"
        );
        let expected_len = (1 + capacity) as u64 * PAGE_SIZE as u64;
        if device.size()? < expected_len {
            device.set_len(expected_len)?;
        }
        Ok(Self { device: Mutex::new(device), capacity })
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    fn offset_of_slot(&self, index: usize) -> u64 {
        (1 + index) as u64 * PAGE_SIZE as u64
    }

    pub fn write_batch(&self, pages: &[Page]) -> Result<(), StorageError> {
        debug_assert!(
            pages.len() <= self.capacity,
            "batch of {} pages exceeds the double-write buffer's {}-slot capacity",
            pages.len(),
            self.capacity
        );
        let device = recover_lock(self.device.lock(), "DoubleWriteBuffer.device");
        let mut entries = Vec::with_capacity(pages.len());
        for (index, page) in pages.iter().enumerate() {
            let mut scratch = *page.data();
            page::stamp_checksum(&mut scratch);
            device.write_at(self.offset_of_slot(index), &scratch)?;
            entries.push((page.id(), crc32(&scratch)));
        }
        let header = self.encode_header(entries.into_iter());
        device.write_at(0, &header)?;
        device.sync_all()?;
        Ok(())
    }

    pub fn clear_batch(&self) -> Result<(), StorageError> {
        let header = self.encode_header(std::iter::empty());
        let device = recover_lock(self.device.lock(), "DoubleWriteBuffer.device");
        device.write_at(0, &header)?;
        device.sync_all()?;
        Ok(())
    }

    fn encode_header(&self, entries: impl Iterator<Item = (PageId, u32)>) -> [u8; PAGE_SIZE] {
        let entries: Vec<(PageId, u32)> = entries.collect();
        let mut buf = [0u8; PAGE_SIZE];
        buf[0..MAGIC.len()].copy_from_slice(MAGIC);
        buf[ENTRY_COUNT_OFFSET..ENTRIES_OFFSET]
            .copy_from_slice(&(entries.len() as u32).to_le_bytes());
        for (i, (id, slot_crc)) in entries.iter().enumerate() {
            let at = ENTRIES_OFFSET + i * ENTRY_SIZE;
            buf[at..at + 4].copy_from_slice(&id.0.to_le_bytes());
            buf[at + 4..at + 8].copy_from_slice(&slot_crc.to_le_bytes());
        }
        let content_end = ENTRIES_OFFSET + entries.len() * ENTRY_SIZE;
        let crc = crc32(&buf[0..content_end]);
        buf[content_end..content_end + 4].copy_from_slice(&crc.to_le_bytes());
        buf
    }

    pub fn read_batch(&self) -> Result<Option<Vec<(PageId, u32)>>, StorageError> {
        let mut header = [0u8; PAGE_SIZE];
        recover_lock(self.device.lock(), "DoubleWriteBuffer.device").read_at(0, &mut header)?;

        if header[0..MAGIC.len()] != *MAGIC {
            return Ok(None);
        }
        let count = read_u32(&header, ENTRY_COUNT_OFFSET) as usize;
        if count == 0 || count > self.capacity {
            return Ok(None);
        }
        let content_end = ENTRIES_OFFSET + count * ENTRY_SIZE;
        if content_end + 4 > PAGE_SIZE {
            return Ok(None);
        }
        let stored_crc = read_u32(&header, content_end);
        if crc32(&header[0..content_end]) != stored_crc {
            return Ok(None);
        }

        let entries = (0..count)
            .map(|i| {
                let at = ENTRIES_OFFSET + i * ENTRY_SIZE;
                (PageId(read_u32(&header, at)), read_u32(&header, at + 4))
            })
            .collect();
        Ok(Some(entries))
    }

    pub fn read_slot(&self, index: usize) -> Result<[u8; PAGE_SIZE], StorageError> {
        let mut buf = [0u8; PAGE_SIZE];
        recover_lock(self.device.lock(), "DoubleWriteBuffer.device")
            .read_at(self.offset_of_slot(index), &mut buf)?;
        Ok(buf)
    }

    #[cfg(any(test, feature = "test-util"))]
    pub fn write_raw_slot(
        &self,
        index: usize,
        content: &[u8; PAGE_SIZE],
    ) -> Result<(), StorageError> {
        let device = recover_lock(self.device.lock(), "DoubleWriteBuffer.device");
        device.write_at(self.offset_of_slot(index), content)?;
        device.sync_all()?;
        Ok(())
    }

    #[cfg(any(test, feature = "test-util"))]
    pub fn write_raw_header_entries(&self, entries: &[(PageId, u32)]) -> Result<(), StorageError> {
        let header = self.encode_header(entries.iter().copied());
        let device = recover_lock(self.device.lock(), "DoubleWriteBuffer.device");
        device.write_at(0, &header)?;
        device.sync_all()?;
        Ok(())
    }
}

fn read_u32(buf: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([buf[at], buf[at + 1], buf[at + 2], buf[at + 3]])
}
