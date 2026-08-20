use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;

use common::PageId;

use crate::error::StorageError;
use crate::page::Page;

/// The 8-byte magic string identifying a FerroDB database file, stored at
/// the start of page 0.
const MAGIC: &[u8; 8] = b"FERRODB\0";

/// The on-disk header format version this build reads and writes.
const HEADER_VERSION: u32 = 1;

/// The byte layout of the page-0 header: magic (8) | version (4) |
/// page_count (4) | catalog_first_page (4), zero-padded to a full page.
mod header {
    pub const MAGIC_RANGE: std::ops::Range<usize> = 0..8;
    pub const VERSION_RANGE: std::ops::Range<usize> = 8..12;
    pub const PAGE_COUNT_RANGE: std::ops::Range<usize> = 12..16;
    pub const CATALOG_FIRST_PAGE_RANGE: std::ops::Range<usize> = 16..20;
}

/// Owns the database file and performs raw, page-granular I/O against it.
/// This is the only module allowed to call into `std::fs` for the main
/// database file; every layer above (buffer pool, heap, B+tree) reads and
/// writes pages through here rather than touching the file directly.
pub struct DiskManager {
    file: File,
    #[allow(dead_code)]
    path: PathBuf,
    page_size: usize,
    next_page_id: u32,
    catalog_first_page: u32,
}

impl DiskManager {
    /// Opens (creating if necessary) the database file at `path`, using
    /// `page_size`-byte pages. A brand-new file gets a fresh page-0 header
    /// written to it; an existing file has its header validated (magic and
    /// version) and `next_page_id` derived from the header's `page_count`.
    pub fn open(path: impl Into<PathBuf>, page_size: usize) -> Result<Self, StorageError> {
        let path = path.into();
        let mut file =
            OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&path)?;
        let file_len = file.metadata()?.len();

        if file_len == 0 {
            let mut manager =
                Self { file, path, page_size, next_page_id: 1, catalog_first_page: u32::MAX };
            manager.write_header()?;
            Ok(manager)
        } else {
            let mut header_buf = vec![0u8; page_size];
            file.seek(SeekFrom::Start(0))?;
            file.read_exact(&mut header_buf)?;

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
                    reason: format!("unsupported header version {version}"),
                });
            }
            let page_count = read_u32(&header_buf, header::PAGE_COUNT_RANGE.start);
            let catalog_first_page = read_u32(&header_buf, header::CATALOG_FIRST_PAGE_RANGE.start);

            Ok(Self { file, path, page_size, next_page_id: page_count, catalog_first_page })
        }
    }

    /// Reads the page at `page_id` from disk into `page`. Reading a page
    /// that was never allocated (at or past the file's current extent) is
    /// an error rather than yielding zeroed bytes.
    pub fn read_page(&mut self, page_id: PageId, page: &mut Page) -> Result<(), StorageError> {
        let offset = self.offset_of(page_id);
        let file_len = self.file.metadata()?.len();
        if offset + self.page_size as u64 > file_len {
            return Err(StorageError::PageNotFound(page_id.0));
        }
        self.file.seek(SeekFrom::Start(offset))?;
        self.file.read_exact(page.data_mut())?;
        Ok(())
    }

    /// Writes `page`'s contents to its slot on disk.
    pub fn write_page(&mut self, page_id: PageId, page: &Page) -> Result<(), StorageError> {
        let offset = self.offset_of(page_id);
        self.file.seek(SeekFrom::Start(offset))?;
        self.file.write_all(page.data())?;
        Ok(())
    }

    /// Allocates a new page id by extending the file, without writing any
    /// tuple data to it yet. Persists the new page count into the header so
    /// a reopen picks the right `next_page_id` back up.
    pub fn allocate_page(&mut self) -> Result<PageId, StorageError> {
        let page_id = PageId(self.next_page_id);
        self.next_page_id += 1;
        let new_len = self.next_page_id as u64 * self.page_size as u64;
        self.file.set_len(new_len)?;
        self.write_header()?;
        Ok(page_id)
    }

    /// Forces every write made so far to durable storage.
    pub fn sync(&mut self) -> Result<(), StorageError> {
        self.file.sync_all()?;
        Ok(())
    }

    /// The configured page size for this database file.
    pub fn page_size(&self) -> usize {
        self.page_size
    }

    /// The first page of the catalog's own storage, as recorded in the
    /// database header, or `None` if the catalog has not been persisted
    /// yet (`u32::MAX` sentinel).
    pub fn catalog_first_page(&self) -> Option<PageId> {
        (self.catalog_first_page != u32::MAX).then_some(PageId(self.catalog_first_page))
    }

    /// Records `page_id` as the catalog's first page in the header, for the
    /// next reopen to pick back up.
    pub fn set_catalog_first_page(&mut self, page_id: PageId) -> Result<(), StorageError> {
        self.catalog_first_page = page_id.0;
        self.write_header()
    }

    fn offset_of(&self, page_id: PageId) -> u64 {
        page_id.0 as u64 * self.page_size as u64
    }

    /// Serializes the current header fields and writes them to page 0.
    fn write_header(&mut self) -> Result<(), StorageError> {
        let mut buf = vec![0u8; self.page_size];
        buf[header::MAGIC_RANGE].copy_from_slice(MAGIC);
        buf[header::VERSION_RANGE].copy_from_slice(&HEADER_VERSION.to_le_bytes());
        buf[header::PAGE_COUNT_RANGE].copy_from_slice(&self.next_page_id.to_le_bytes());
        buf[header::CATALOG_FIRST_PAGE_RANGE]
            .copy_from_slice(&self.catalog_first_page.to_le_bytes());

        self.file.seek(SeekFrom::Start(0))?;
        self.file.write_all(&buf)?;
        Ok(())
    }
}

fn read_u32(buf: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([buf[at], buf[at + 1], buf[at + 2], buf[at + 3]])
}
