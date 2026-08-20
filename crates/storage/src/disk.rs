use std::fs::File;
use std::path::PathBuf;

use common::PageId;

use crate::error::StorageError;
use crate::page::Page;

/// Owns the database file and performs raw, page-granular I/O against it.
/// This is the only module allowed to call into `std::fs` for the main
/// database file; every layer above (buffer pool, heap, B+tree) reads and
/// writes pages through here rather than touching the file directly.
pub struct DiskManager {
    #[allow(dead_code)]
    file: File,
    #[allow(dead_code)]
    path: PathBuf,
    page_size: usize,
    #[allow(dead_code)]
    next_page_id: u32,
}

impl DiskManager {
    /// Opens (creating if necessary) the database file at `path`, using
    /// `page_size`-byte pages.
    pub fn open(path: impl Into<PathBuf>, page_size: usize) -> Result<Self, StorageError> {
        let _ = (path, page_size);
        todo!("open or create the file, determine next_page_id from file length")
    }

    /// Reads the page at `page_id` from disk into `page`.
    pub fn read_page(&mut self, page_id: PageId, page: &mut Page) -> Result<(), StorageError> {
        let _ = (page_id, page);
        todo!("seek to page_id * page_size and read page_size bytes")
    }

    /// Writes `page`'s contents to its slot on disk.
    pub fn write_page(&mut self, page_id: PageId, page: &Page) -> Result<(), StorageError> {
        let _ = (page_id, page);
        todo!("seek to page_id * page_size and write page_size bytes, then fsync")
    }

    /// Allocates a new page id by extending the file, without writing any
    /// data to it yet.
    pub fn allocate_page(&mut self) -> Result<PageId, StorageError> {
        todo!("bump next_page_id and extend the file to fit it")
    }

    /// The configured page size for this database file.
    pub fn page_size(&self) -> usize {
        self.page_size
    }
}
