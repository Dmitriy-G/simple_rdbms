use std::fs::OpenOptions;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

use common::PageId;
use common::sync::recover_lock;

use crate::block_device::{BlockDevice, FileDevice};
use crate::error::StorageError;
use crate::page::{self, CHECKSUM_RANGE, Page};

const MAGIC: &[u8; 8] = b"FERRODB\0";

const HEADER_VERSION: u32 = 9;

pub(crate) mod header {
    pub const MAGIC_RANGE: std::ops::Range<usize> = 12..20;
    pub const VERSION_RANGE: std::ops::Range<usize> = 20..24;
    pub const CATALOG_FIRST_PAGE_RANGE: std::ops::Range<usize> = 24..28;
    pub const PAGE_SIZE_RANGE: std::ops::Range<usize> = 28..32;
    pub const LAST_CHECKPOINT_LSN_RANGE: std::ops::Range<usize> = 32..40;
    pub const INDEX_CATALOG_FIRST_PAGE_RANGE: std::ops::Range<usize> = 40..44;
}

pub struct DiskManager {
    device: Box<dyn BlockDevice>,
    #[allow(dead_code)]
    path: Option<PathBuf>,
    page_size: usize,
    next_page_id: AtomicU32,
    extended_len: Mutex<u64>,
}

impl DiskManager {
    pub fn open(path: impl Into<PathBuf>, page_size: usize) -> Result<Self, StorageError> {
        let path = path.into();
        let file =
            OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&path)?;
        Self::open_with_device(Box::new(FileDevice::new(file)), page_size, Some(path))
    }

    pub fn open_with_device(
        device: Box<dyn BlockDevice>,
        page_size: usize,
        path: Option<PathBuf>,
    ) -> Result<Self, StorageError> {
        let device_len = device.size()?;

        if device_len == 0 {
            let manager = Self {
                device,
                path,
                page_size,
                next_page_id: AtomicU32::new(1),
                extended_len: Mutex::new(page_size as u64),
            };
            manager.write_header()?;
            Ok(manager)
        } else {
            if device_len % page_size as u64 != 0 {
                let whole_pages = device_len / page_size as u64;
                return Err(StorageError::TruncatedFile {
                    actual: device_len,
                    expected: whole_pages * page_size as u64,
                    page_size,
                });
            }

            let mut header_buf = vec![0u8; header::PAGE_SIZE_RANGE.end];
            device.read_at(0, &mut header_buf)?;

            if &header_buf[header::MAGIC_RANGE] != MAGIC.as_slice() {
                return Err(StorageError::CorruptPage {
                    page_id: 0,
                    reason: "bad magic in database header".to_string(),
                });
            }
            let version = read_u32(&header_buf, header::VERSION_RANGE.start);
            if version != HEADER_VERSION {
                return Err(StorageError::CorruptPage {
                    page_id: 0,
                    reason: format!(
                        "unsupported on-disk format version {version}: this build reads and \
                         writes version {HEADER_VERSION}"
                    ),
                });
            }
            let stored_page_size = read_u32(&header_buf, header::PAGE_SIZE_RANGE.start);
            if stored_page_size as usize != page_size {
                return Err(StorageError::CorruptPage {
                    page_id: 0,
                    reason: format!(
                        "page size mismatch: database was created with page_size \
                         {stored_page_size}, but {page_size} was requested"
                    ),
                });
            }

            let next_page_id = (device_len / page_size as u64) as u32;
            Ok(Self {
                device,
                path,
                page_size,
                next_page_id: AtomicU32::new(next_page_id),
                extended_len: Mutex::new(device_len),
            })
        }
    }

    pub(crate) fn read_page_unchecked(
        &self,
        page_id: PageId,
        page: &mut Page,
    ) -> Result<(), StorageError> {
        let offset = self.offset_of(page_id);
        let device_len = self.device.size()?;
        if offset + self.page_size as u64 > device_len {
            return Err(StorageError::PageNotFound(page_id.0));
        }
        self.device.read_at(offset, page.data_mut())?;
        metrics::counter!("disk_pages_read_total").increment(1);
        Ok(())
    }

    pub fn read_page(&self, page_id: PageId, page: &mut Page) -> Result<(), StorageError> {
        self.read_page_unchecked(page_id, page)?;
        if !page::checksum_ok(page.data()) {
            let expected = read_u32(page.data(), CHECKSUM_RANGE.start);
            let actual = page::checksum_of(page.data());
            return Err(StorageError::ChecksumMismatch { page_id: page_id.0, expected, actual });
        }
        Ok(())
    }

    pub fn write_page(&self, page_id: PageId, page: &Page) -> Result<(), StorageError> {
        let offset = self.offset_of(page_id);
        let mut scratch = *page.data();
        page::stamp_checksum(&mut scratch);
        self.device.write_at(offset, &scratch)?;
        metrics::counter!("disk_pages_written_total").increment(1);
        Ok(())
    }

    pub fn allocate_page(&self) -> Result<PageId, StorageError> {
        let page_id = self.next_page_id.fetch_add(1, Ordering::AcqRel);
        let target_len = (page_id as u64 + 1) * self.page_size as u64;
        self.extend_to_at_least(target_len)?;
        Ok(PageId(page_id))
    }

    pub fn ensure_allocated(&self, page_id: PageId) -> Result<(), StorageError> {
        let needed = page_id.0 + 1;
        let mut current = self.next_page_id.load(Ordering::Acquire);
        let mut bumped = false;
        while current < needed {
            match self.next_page_id.compare_exchange_weak(
                current,
                needed,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    bumped = true;
                    break;
                }
                Err(observed) => current = observed,
            }
        }
        if bumped {
            let target_len = needed as u64 * self.page_size as u64;
            self.extend_to_at_least(target_len)?;
        }
        Ok(())
    }

    fn extend_to_at_least(&self, target_len: u64) -> Result<(), StorageError> {
        let mut extended = recover_lock(self.extended_len.lock(), "DiskManager.extended_len");
        if *extended < target_len {
            self.device.set_len(target_len)?;
            *extended = target_len;
        }
        Ok(())
    }

    pub fn sync(&self) -> Result<(), StorageError> {
        self.device.sync_all()?;
        Ok(())
    }

    pub fn page_size(&self) -> usize {
        self.page_size
    }

    fn offset_of(&self, page_id: PageId) -> u64 {
        page_id.0 as u64 * self.page_size as u64
    }

    fn write_header(&self) -> Result<(), StorageError> {
        let mut buf = [0u8; crate::page::PAGE_SIZE];
        buf[header::MAGIC_RANGE].copy_from_slice(MAGIC);
        buf[header::VERSION_RANGE].copy_from_slice(&HEADER_VERSION.to_le_bytes());
        buf[header::CATALOG_FIRST_PAGE_RANGE].copy_from_slice(&u32::MAX.to_le_bytes());
        buf[header::INDEX_CATALOG_FIRST_PAGE_RANGE].copy_from_slice(&u32::MAX.to_le_bytes());
        buf[header::PAGE_SIZE_RANGE].copy_from_slice(&(self.page_size as u32).to_le_bytes());

        page::stamp_checksum(&mut buf);

        self.device.write_at(0, &buf)?;
        Ok(())
    }
}

fn read_u32(buf: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([buf[at], buf[at + 1], buf[at + 2], buf[at + 3]])
}
