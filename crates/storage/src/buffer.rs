use std::cell::{Cell, RefCell, UnsafeCell};
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
///
/// Every field here is behind interior mutability (`RefCell`/`Cell`/
/// `UnsafeCell`) so that `fetch_page`/`new_page` can take `&self` rather
/// than `&mut self`: a `PageGuard`'s whole point is to let multiple pages
/// be pinned at once, which isn't possible if borrowing one page ties up
/// an exclusive borrow of the whole pool. Each lock guards a specific
/// piece of bookkeeping:
///   - `disk_manager`: serializes the (rare) actual disk I/O calls.
///   - `page_table`/`frame_page`: the two-way `PageId <-> FrameId` mapping.
///   - `pin_counts`/`dirty`: per-frame `Cell`s, one independent lock each.
///   - `replacer`: the eviction policy's own bookkeeping.
///   - `frames`: an `UnsafeCell` per frame. Safety relies on the pin-count
///     protocol above: a frame is only ever mutated by code that holds it
///     pinned (a live `PageGuard`, or the pool itself while the frame is
///     unpinned and thus inaccessible to any guard), so two overlapping
///     `&mut` borrows of the same frame's bytes never occur.
pub struct BufferPool {
    disk_manager: RefCell<DiskManager>,
    frames: Vec<UnsafeCell<Page>>,
    page_table: RefCell<HashMap<PageId, FrameId>>,
    frame_page: Vec<Cell<Option<PageId>>>,
    pin_counts: Vec<Cell<usize>>,
    dirty: Vec<Cell<bool>>,
    free_list: RefCell<Vec<FrameId>>,
    replacer: RefCell<Box<dyn Replacer>>,
}

impl BufferPool {
    /// Creates a buffer pool of `pool_size` frames over `disk_manager`,
    /// using `replacer` to choose eviction victims.
    pub fn new(disk_manager: DiskManager, pool_size: usize, replacer: Box<dyn Replacer>) -> Self {
        let frames = (0..pool_size).map(|_| UnsafeCell::new(Page::new(PageId(0)))).collect();
        let frame_page = (0..pool_size).map(|_| Cell::new(None)).collect();
        let pin_counts = (0..pool_size).map(|_| Cell::new(0)).collect();
        let dirty = (0..pool_size).map(|_| Cell::new(false)).collect();
        let free_list = RefCell::new((0..pool_size as u32).map(FrameId).collect());

        Self {
            disk_manager: RefCell::new(disk_manager),
            frames,
            page_table: RefCell::new(HashMap::new()),
            frame_page,
            pin_counts,
            dirty,
            free_list,
            replacer: RefCell::new(replacer),
        }
    }

    /// Fetches the page `page_id`, pinning it and returning a guard. If the
    /// page is not already resident, brings it in from disk, evicting a
    /// victim frame via the replacer if none is free.
    pub fn fetch_page(&self, page_id: PageId) -> Result<PageGuard<'_>, StorageError> {
        if let Some(&frame_id) = self.page_table.borrow().get(&page_id) {
            self.pin(frame_id);
            return Ok(PageGuard { page_id, frame_id, pool: self });
        }

        let frame_id = self.allocate_frame()?;
        let mut page = Page::new(page_id);
        self.disk_manager.borrow_mut().read_page(page_id, &mut page)?;
        self.install(frame_id, page_id, page, false);
        self.pin(frame_id);
        Ok(PageGuard { page_id, frame_id, pool: self })
    }

    /// Allocates a brand-new page via the disk manager and pins it in the
    /// pool, returning a guard to it.
    pub fn new_page(&self) -> Result<(PageId, PageGuard<'_>), StorageError> {
        let page_id = self.disk_manager.borrow_mut().allocate_page()?;
        let frame_id = self.allocate_frame()?;
        self.install(frame_id, page_id, Page::new(page_id), true);
        self.pin(frame_id);
        Ok((page_id, PageGuard { page_id, frame_id, pool: self }))
    }

    /// Unpins `page_id`, optionally marking its frame dirty. A page becomes
    /// eligible for eviction once its pin count drops to zero.
    pub fn unpin_page(&self, page_id: PageId, is_dirty: bool) -> Result<(), StorageError> {
        let frame_id = self.frame_of(page_id)?;
        if is_dirty {
            self.dirty[frame_id.0 as usize].set(true);
        }
        self.unpin_frame(frame_id);
        Ok(())
    }

    /// Flushes `page_id` to disk if its frame is dirty.
    pub fn flush_page(&self, page_id: PageId) -> Result<(), StorageError> {
        let frame_id = self.frame_of(page_id)?;
        if self.dirty[frame_id.0 as usize].get() {
            self.flush_frame(frame_id, page_id)?;
        }
        Ok(())
    }

    /// Flushes every dirty resident page to disk.
    pub fn flush_all(&self) -> Result<(), StorageError> {
        let page_ids: Vec<PageId> = self.page_table.borrow().keys().copied().collect();
        for page_id in page_ids {
            self.flush_page(page_id)?;
        }
        Ok(())
    }

    /// The catalog's first page, as recorded in the database header, or
    /// `None` if the catalog has not been persisted yet.
    pub fn catalog_first_page(&self) -> Option<PageId> {
        self.disk_manager.borrow().catalog_first_page()
    }

    /// Records `page_id` as the catalog's first page in the database
    /// header, so a reopen can find it again.
    pub fn set_catalog_first_page(&self, page_id: PageId) -> Result<(), StorageError> {
        self.disk_manager.borrow_mut().set_catalog_first_page(page_id)
    }

    /// Finds a frame to hold a newly-fetched or newly-allocated page:
    /// reuses a never-used frame if one is free, otherwise asks the
    /// replacer for an evictable victim, flushing it first if dirty.
    fn allocate_frame(&self) -> Result<FrameId, StorageError> {
        if let Some(frame_id) = self.free_list.borrow_mut().pop() {
            return Ok(frame_id);
        }

        let frame_id =
            self.replacer.borrow_mut().evict().ok_or(StorageError::BufferPoolExhausted)?;

        let idx = frame_id.0 as usize;
        if let Some(victim_page_id) = self.frame_page[idx].get() {
            if self.dirty[idx].get() {
                self.flush_frame(frame_id, victim_page_id)?;
            }
            self.page_table.borrow_mut().remove(&victim_page_id);
            self.frame_page[idx].set(None);
        }
        Ok(frame_id)
    }

    /// Installs `page` into `frame_id`'s slot and records it under
    /// `page_id` in the page table. Only ever called on a frame that was
    /// just freed or evicted, so no live guard can be observing its bytes.
    fn install(&self, frame_id: FrameId, page_id: PageId, page: Page, dirty: bool) {
        let idx = frame_id.0 as usize;
        // SAFETY: `frame_id` came from the free list or the replacer's
        // `evict`, both of which only hand back frames with a pin count of
        // zero, so no `PageGuard` holds a reference into this frame.
        unsafe {
            *self.frames[idx].get() = page;
        }
        self.frame_page[idx].set(Some(page_id));
        self.page_table.borrow_mut().insert(page_id, frame_id);
        self.dirty[idx].set(dirty);
    }

    fn pin(&self, frame_id: FrameId) {
        let idx = frame_id.0 as usize;
        self.pin_counts[idx].set(self.pin_counts[idx].get() + 1);
        let mut replacer = self.replacer.borrow_mut();
        replacer.record_access(frame_id);
        replacer.set_evictable(frame_id, false);
    }

    fn unpin_frame(&self, frame_id: FrameId) {
        let idx = frame_id.0 as usize;
        let count = self.pin_counts[idx].get();
        if count == 0 {
            return;
        }
        let remaining = count - 1;
        self.pin_counts[idx].set(remaining);
        if remaining == 0 {
            self.replacer.borrow_mut().set_evictable(frame_id, true);
        }
    }

    fn flush_frame(&self, frame_id: FrameId, page_id: PageId) -> Result<(), StorageError> {
        let idx = frame_id.0 as usize;
        // SAFETY: called either on an unpinned victim frame (about to be
        // evicted, so nothing else touches it) or via `flush_page`, which
        // only needs read access to bytes a caller's own guard already
        // has permission to read.
        let page = unsafe { &*self.frames[idx].get() };
        self.disk_manager.borrow_mut().write_page(page_id, page)?;
        self.dirty[idx].set(false);
        Ok(())
    }

    fn frame_of(&self, page_id: PageId) -> Result<FrameId, StorageError> {
        self.page_table.borrow().get(&page_id).copied().ok_or(StorageError::PageNotFound(page_id.0))
    }
}

impl<'pool> PageGuard<'pool> {
    /// Read-only access to the pinned page.
    pub fn page(&self) -> &Page {
        let idx = self.frame_id.0 as usize;
        // SAFETY: this guard's existence means the frame is pinned, so the
        // pool will not reassign or evict it out from under this borrow.
        unsafe { &*self.pool.frames[idx].get() }
    }

    /// Mutable access to the pinned page, marking the frame dirty.
    ///
    /// Note: this pool does not yet enforce single-writer access to a page
    /// (that arrives with page-level locking in a later milestone). A
    /// caller that fetches the same page twice and calls `page_mut` on
    /// both guards concurrently would alias `&mut` references to the same
    /// bytes; the buffer pool relies on callers not doing that, same as
    /// it relies on the whole engine being single-threaded for now.
    pub fn page_mut(&mut self) -> &mut Page {
        let idx = self.frame_id.0 as usize;
        self.pool.dirty[idx].set(true);
        // SAFETY: this frame is pinned for the lifetime of this guard, so
        // the pool will never evict or reassign it while this borrow is
        // live.
        unsafe { &mut *self.pool.frames[idx].get() }
    }
}

impl Drop for PageGuard<'_> {
    fn drop(&mut self) {
        self.pool.unpin_frame(self.frame_id);
    }
}
