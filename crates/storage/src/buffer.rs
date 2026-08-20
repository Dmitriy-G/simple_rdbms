use std::collections::HashMap;

use common::{FrameId, PageId};

use crate::disk::DiskManager;
use crate::error::StorageError;
use crate::page::{Page, PageGuard};
use crate::replacer::Replacer;

/// Caches a bounded number of pages in memory, mediating every access to
/// pages between the disk manager and the layers above (heap files,
/// B+tree, WAL). A page is pinned for as long as a caller holds a
/// `PageGuard` to it; once unpinned it becomes a candidate for eviction,
/// chosen by the configured `Replacer`, when a new page must be loaded and
/// no frame is free.
pub struct BufferPool {
    #[allow(dead_code)]
    disk_manager: DiskManager,
    #[allow(dead_code)]
    frames: Vec<Page>,
    #[allow(dead_code)]
    page_table: HashMap<PageId, FrameId>,
    #[allow(dead_code)]
    pin_counts: Vec<usize>,
    #[allow(dead_code)]
    dirty: Vec<bool>,
    #[allow(dead_code)]
    replacer: Box<dyn Replacer>,
}

impl BufferPool {
    /// Creates a buffer pool of `pool_size` frames over `disk_manager`,
    /// using `replacer` to choose eviction victims.
    pub fn new(disk_manager: DiskManager, pool_size: usize, replacer: Box<dyn Replacer>) -> Self {
        let _ = &disk_manager;
        let _ = pool_size;
        let _ = &replacer;
        todo!("allocate pool_size zeroed frames and initialize page_table/pin_counts/dirty")
    }

    /// Fetches the page `page_id`, pinning it and returning a guard. If the
    /// page is not already resident, brings it in from disk, evicting a
    /// victim frame via the replacer if none is free.
    pub fn fetch_page(&mut self, page_id: PageId) -> Result<PageGuard<'_>, StorageError> {
        let _ = page_id;
        todo!("look up page_table; on miss, evict a victim frame and read from disk")
    }

    /// Allocates a brand-new page via the disk manager and pins it in the
    /// pool, returning a guard to it.
    pub fn new_page(&mut self) -> Result<PageGuard<'_>, StorageError> {
        todo!("call disk_manager.allocate_page and install it into a free or evicted frame")
    }

    /// Unpins `page_id`, optionally marking its frame dirty. A page becomes
    /// eligible for eviction once its pin count drops to zero.
    pub fn unpin_page(&mut self, page_id: PageId, is_dirty: bool) -> Result<(), StorageError> {
        let _ = (page_id, is_dirty);
        todo!("decrement the frame's pin count and mark it evictable at zero")
    }

    /// Flushes `page_id` to disk if its frame is dirty.
    pub fn flush_page(&mut self, page_id: PageId) -> Result<(), StorageError> {
        let _ = page_id;
        todo!("write the frame's bytes to disk via the disk manager and clear the dirty flag")
    }

    /// Flushes every dirty resident page to disk.
    pub fn flush_all(&mut self) -> Result<(), StorageError> {
        todo!("call flush_page for every page currently in page_table")
    }
}
