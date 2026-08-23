use std::cell::{Cell, RefCell, UnsafeCell};
use std::collections::HashMap;

use common::{FrameId, Lsn, PageId, TxnId};

use crate::disk::{DiskManager, header};
use crate::dwb::DoubleWriteBuffer;
use crate::error::StorageError;
use crate::page::{Page, PageGuard};
use crate::replacer::Replacer;
use crate::wal::{LogManager, LogRecord, LogRecordKind};

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
///   - `dwb`: the double-write buffer every dirty page is copied into
///     (and that copy synced) before the page itself reaches its real
///     location - see `flush_pages`.
///   - `log_manager`: the write-ahead log every page mutation is recorded
///     into before it reaches this pool's own bytes (see `PageGuard::write`
///     and `flush_pages`).
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
    dwb: RefCell<DoubleWriteBuffer>,
    log_manager: RefCell<LogManager>,
    frames: Vec<UnsafeCell<Page>>,
    page_table: RefCell<HashMap<PageId, FrameId>>,
    frame_page: Vec<Cell<Option<PageId>>>,
    pin_counts: Vec<Cell<usize>>,
    /// `Some(lsn)` for a dirty frame, where `lsn` is the LSN of the record
    /// that *first* dirtied it since its last flush - the `recovery_lsn`
    /// ARIES's Dirty Page Table needs (see `dirty_page_table`). `None` for
    /// a clean frame.
    dirty_since_lsn: Vec<Cell<Option<Lsn>>>,
    free_list: RefCell<Vec<FrameId>>,
    replacer: RefCell<Box<dyn Replacer>>,
    /// Counts `fetch_page` calls, for tests to assert on how many pages an
    /// operation actually touched (e.g. that a lazy scan fetched only the
    /// page it needed). Not present in ordinary builds.
    #[cfg(any(test, feature = "test-util"))]
    fetch_count: Cell<usize>,
    /// Records, in order, every page actually written to disk by
    /// `flush_frame`, alongside that page's `page_lsn` and the log's
    /// `durable_lsn` at that exact moment - what the WAL-ordering
    /// invariant test checks against. Not present in ordinary builds.
    #[cfg(any(test, feature = "test-util"))]
    write_observations: RefCell<Vec<WriteObservation>>,
}

/// One observed disk write, for the WAL-ordering invariant test: at the
/// moment `page_id` was written to disk, its `page_lsn` and the log's
/// `durable_lsn` at that instant. The write-ahead rule requires
/// `durable_lsn >= page_lsn` for every entry.
#[cfg(any(test, feature = "test-util"))]
#[derive(Debug, Clone, Copy)]
pub struct WriteObservation {
    /// The page written to disk.
    pub page_id: PageId,
    /// That page's `page_lsn` at the moment of the write.
    pub page_lsn: Lsn,
    /// The log's `durable_lsn` at the moment of the write.
    pub durable_lsn: Lsn,
}

impl BufferPool {
    /// Creates a buffer pool of `pool_size` frames over `disk_manager`,
    /// using `replacer` to choose eviction victims, logging every page
    /// mutation through `log_manager` before it is applied, and protecting
    /// every flush against a torn write through `dwb` (see `flush_pages`).
    pub fn new(
        disk_manager: DiskManager,
        dwb: DoubleWriteBuffer,
        log_manager: LogManager,
        pool_size: usize,
        replacer: Box<dyn Replacer>,
    ) -> Self {
        let frames = (0..pool_size).map(|_| UnsafeCell::new(Page::new(PageId(0)))).collect();
        let frame_page = (0..pool_size).map(|_| Cell::new(None)).collect();
        let pin_counts = (0..pool_size).map(|_| Cell::new(0)).collect();
        let dirty_since_lsn = (0..pool_size).map(|_| Cell::new(None)).collect();
        let free_list = RefCell::new((0..pool_size as u32).map(FrameId).collect());

        Self {
            disk_manager: RefCell::new(disk_manager),
            dwb: RefCell::new(dwb),
            log_manager: RefCell::new(log_manager),
            frames,
            page_table: RefCell::new(HashMap::new()),
            frame_page,
            pin_counts,
            dirty_since_lsn,
            free_list,
            replacer: RefCell::new(replacer),
            #[cfg(any(test, feature = "test-util"))]
            fetch_count: Cell::new(0),
            #[cfg(any(test, feature = "test-util"))]
            write_observations: RefCell::new(Vec::new()),
        }
    }

    /// The number of `fetch_page` calls made so far. Test-only.
    #[cfg(any(test, feature = "test-util"))]
    pub fn fetch_count(&self) -> usize {
        self.fetch_count.get()
    }

    /// Resets the `fetch_page` call counter to zero. Test-only.
    #[cfg(any(test, feature = "test-util"))]
    pub fn reset_fetch_count(&self) {
        self.fetch_count.set(0);
    }

    /// Every disk write `flush_frame` has performed so far, in order.
    /// Test-only.
    #[cfg(any(test, feature = "test-util"))]
    pub fn write_observations(&self) -> Vec<WriteObservation> {
        self.write_observations.borrow().clone()
    }

    /// Fetches the page `page_id`, pinning it and returning a guard. If the
    /// page is not already resident, brings it in from disk, evicting a
    /// victim frame via the replacer if none is free.
    pub fn fetch_page(&self, page_id: PageId) -> Result<PageGuard<'_>, StorageError> {
        #[cfg(any(test, feature = "test-util"))]
        self.fetch_count.set(self.fetch_count.get() + 1);
        if let Some(&frame_id) = self.page_table.borrow().get(&page_id) {
            self.pin(frame_id);
            return Ok(PageGuard { page_id, frame_id, pool: self });
        }

        let frame_id = self.allocate_frame()?;
        let mut page = Page::new(page_id);
        self.disk_manager.borrow_mut().read_page(page_id, &mut page)?;
        self.install(frame_id, page_id, page, None);
        self.pin(frame_id);
        Ok(PageGuard { page_id, frame_id, pool: self })
    }

    /// Allocates a brand-new page via the disk manager and pins it in the
    /// pool, returning a guard to it. Logs an `AllocPage` record on behalf
    /// of `txn_id` before installing the page.
    pub fn new_page(&self, txn_id: TxnId) -> Result<(PageId, PageGuard<'_>), StorageError> {
        let page_id = self.disk_manager.borrow_mut().allocate_page()?;
        let lsn = self.append_log(txn_id, LogRecordKind::AllocPage { page_id })?;
        let frame_id = self.allocate_frame()?;
        self.install(frame_id, page_id, Page::new(page_id), Some(lsn));
        self.pin(frame_id);
        Ok((page_id, PageGuard { page_id, frame_id, pool: self }))
    }

    /// Flushes `page_id` to disk if its frame is dirty.
    pub fn flush_page(&self, page_id: PageId) -> Result<(), StorageError> {
        let frame_id = self.frame_of(page_id)?;
        if self.dirty_since_lsn[frame_id.0 as usize].get().is_some() {
            self.flush_pages(&[(frame_id, page_id)])?;
        }
        Ok(())
    }

    /// Flushes every dirty resident page to disk, batching them through the
    /// double-write buffer in groups of at most `dwb.capacity()` pages -
    /// one double-write round trip per group rather than per page, which is
    /// where batching actually pays for the doubled writes `flush_pages`
    /// costs (see its own doc comment).
    pub fn flush_all(&self) -> Result<(), StorageError> {
        let dirty: Vec<(FrameId, PageId)> = self
            .frame_page
            .iter()
            .enumerate()
            .filter_map(|(idx, page_id)| {
                let page_id = page_id.get()?;
                self.dirty_since_lsn[idx].get().is_some().then_some((FrameId(idx as u32), page_id))
            })
            .collect();

        // `DoubleWriteBuffer::open` rejects `capacity == 0`, so this is
        // always a positive chunk size - no `.max(1)` paper-over needed.
        let capacity = self.dwb.borrow().capacity();
        for batch in dirty.chunks(capacity) {
            self.flush_pages(batch)?;
        }
        Ok(())
    }

    /// Forces every write made so far to durable storage. Callers should
    /// `flush_all` first; this only syncs whatever has already reached the
    /// disk manager.
    pub fn sync(&self) -> Result<(), StorageError> {
        self.disk_manager.borrow_mut().sync()
    }

    /// The catalog's first page, as recorded in the database header (page
    /// 0), or `None` if the catalog has not been persisted yet. Reads page 0
    /// through the ordinary buffer-pool path - fetching it like any other
    /// page - rather than a value cached before crash recovery ran, so this
    /// always reflects the current, possibly just-redone-or-undone state.
    pub fn catalog_first_page(&self) -> Result<Option<PageId>, StorageError> {
        let guard = self.fetch_page(PageId(0))?;
        let raw = read_u32(guard.page().data(), header::CATALOG_FIRST_PAGE_RANGE.start);
        Ok((raw != u32::MAX).then_some(PageId(raw)))
    }

    /// Records `page_id` as the catalog's first page in the database header
    /// (page 0), on behalf of `txn_id` - an ordinary, logged
    /// `PageGuard::write` like any other page mutation, so it is redone or
    /// undone exactly like the catalog heap allocation it accompanies rather
    /// than racing ahead of it as an unlogged direct write.
    pub fn set_catalog_first_page(
        &self,
        txn_id: TxnId,
        page_id: PageId,
    ) -> Result<(), StorageError> {
        let mut guard = self.fetch_page(PageId(0))?;
        guard.write(txn_id, header::CATALOG_FIRST_PAGE_RANGE.start, &page_id.0.to_le_bytes())
    }

    /// Appends `kind` to the write-ahead log on behalf of `txn_id`,
    /// returning its assigned LSN. Does not by itself make the record
    /// durable; call `flush_log` (or `flush_log_all`) for that.
    pub fn append_log(&self, txn_id: TxnId, kind: LogRecordKind) -> Result<Lsn, StorageError> {
        self.log_manager.borrow_mut().append(LogRecord { txn_id, kind })
    }

    /// Forces every log record up to and including `up_to` to disk.
    pub fn flush_log(&self, up_to: Lsn) -> Result<(), StorageError> {
        self.log_manager.borrow_mut().flush(up_to)
    }

    /// Forces every log record appended so far to disk, regardless of
    /// `up_to`. Used at clean shutdown.
    pub fn flush_log_all(&self) -> Result<(), StorageError> {
        self.log_manager.borrow_mut().flush_all()
    }

    /// The write-ahead log's current durable LSN.
    pub fn durable_lsn(&self) -> Lsn {
        self.log_manager.borrow().durable_lsn()
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
            if self.dirty_since_lsn[idx].get().is_some() {
                self.flush_pages(&[(frame_id, victim_page_id)])?;
            }
            self.page_table.borrow_mut().remove(&victim_page_id);
            self.frame_page[idx].set(None);
        }
        Ok(frame_id)
    }

    /// Installs `page` into `frame_id`'s slot and records it under
    /// `page_id` in the page table. Only ever called on a frame that was
    /// just freed or evicted, so no live guard can be observing its bytes.
    /// `dirty_since_lsn` is `Some(lsn)` if the page is already dirty as of
    /// installation (a brand-new page, dirtied by its own `AllocPage`
    /// record), or `None` for a page freshly read from disk.
    fn install(
        &self,
        frame_id: FrameId,
        page_id: PageId,
        page: Page,
        dirty_since_lsn: Option<Lsn>,
    ) {
        let idx = frame_id.0 as usize;
        // SAFETY: `frame_id` came from the free list or the replacer's
        // `evict`, both of which only hand back frames with a pin count of
        // zero, so no `PageGuard` holds a reference into this frame.
        unsafe {
            *self.frames[idx].get() = page;
        }
        self.frame_page[idx].set(Some(page_id));
        self.page_table.borrow_mut().insert(page_id, frame_id);
        self.dirty_since_lsn[idx].set(dirty_since_lsn);
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
        debug_assert!(count > 0, "pin count underflow on frame {frame_id:?}");
        if count == 0 {
            return;
        }
        let remaining = count - 1;
        self.pin_counts[idx].set(remaining);
        if remaining == 0 {
            self.replacer.borrow_mut().set_evictable(frame_id, true);
        }
    }

    /// Flushes a batch of currently-dirty, resident pages to disk through
    /// the double-write buffer. Every entry in `pages` must already be
    /// dirty and safe from concurrent reassignment - an eviction victim
    /// (about to be reused, so nothing else touches it) or a page
    /// `flush_page`/`flush_all` already found dirty; `flush_pages` itself
    /// does not re-check either condition.
    ///
    /// Implements the write-ahead rule (the log must be durable to the
    /// batch's *highest* `page_lsn` before anything reaches the
    /// double-write buffer - not before each page individually, and not
    /// merely before the real file, which is what made a torn write
    /// unrecoverable before this existed) and the double-write protocol
    /// itself: write every page's image to `dwb` and sync it (so the batch
    /// is now recoverable even if nothing below has happened yet), only
    /// then write every page to its real location and sync that, then
    /// clear `dwb`. A crash at any point along the way leaves either a
    /// real page untouched by this batch (nothing below the point of the
    /// crash ran) or a torn real page with an intact `dwb` copy
    /// `recovery::recover_double_write` can restore from - never a torn
    /// real page with no way back.
    fn flush_pages(&self, pages: &[(FrameId, PageId)]) -> Result<(), StorageError> {
        if pages.is_empty() {
            return Ok(());
        }
        debug_assert!(
            pages.len() <= self.dwb.borrow().capacity(),
            "flush batch of {} pages exceeds the double-write buffer's capacity",
            pages.len()
        );

        // SAFETY: every frame named here is either an eviction victim
        // (about to be reused, so nothing else touches it) or a page
        // `flush_page`/`flush_all` already confirmed dirty and resident -
        // both only need read access to bytes a caller's own guard already
        // has permission to read, same as the original single-page flush.
        let snapshot: Vec<Page> = pages
            .iter()
            .map(|&(frame_id, _)| unsafe { (*self.frames[frame_id.0 as usize].get()).clone() })
            .collect();

        let max_page_lsn = snapshot.iter().map(Page::page_lsn).max().unwrap_or(Lsn(0));
        self.log_manager.borrow_mut().flush(max_page_lsn)?;
        let durable_lsn = self.log_manager.borrow().durable_lsn();
        debug_assert!(
            durable_lsn >= max_page_lsn,
            "batch reached disk with max page_lsn {max_page_lsn:?} ahead of durable_lsn \
             {durable_lsn:?}"
        );
        #[cfg(any(test, feature = "test-util"))]
        for page in &snapshot {
            self.write_observations.borrow_mut().push(WriteObservation {
                page_id: page.id(),
                page_lsn: page.page_lsn(),
                durable_lsn,
            });
        }

        self.dwb.borrow_mut().write_batch(&snapshot)?;

        {
            let mut disk = self.disk_manager.borrow_mut();
            for page in &snapshot {
                disk.write_page(page.id(), page)?;
            }
            disk.sync()?;
        }

        self.dwb.borrow_mut().clear_batch()?;

        for &(frame_id, _) in pages {
            self.dirty_since_lsn[frame_id.0 as usize].set(None);
        }
        Ok(())
    }

    fn frame_of(&self, page_id: PageId) -> Result<FrameId, StorageError> {
        self.page_table.borrow().get(&page_id).copied().ok_or(StorageError::PageNotFound(page_id.0))
    }

    /// The Dirty Page Table: every currently-dirty resident page and its
    /// `recovery_lsn` (the LSN of the record that first dirtied it since
    /// its last flush). A checkpoint snapshot of this is what lets
    /// recovery's Redo pass start from the oldest change that might still
    /// be missing from disk, rather than the beginning of the log.
    pub fn dirty_page_table(&self) -> Vec<(PageId, Lsn)> {
        self.frame_page
            .iter()
            .zip(self.dirty_since_lsn.iter())
            .filter_map(|(page_id, dirty_lsn)| Some((page_id.get()?, dirty_lsn.get()?)))
            .collect()
    }

    /// The LSN of the most recent checkpoint's `CheckpointBegin` record, as
    /// recorded in the database header (page 0), or `None` if no checkpoint
    /// has been taken yet. Reads page 0 through the ordinary buffer-pool
    /// path, like `catalog_first_page`.
    pub fn last_checkpoint_lsn(&self) -> Result<Option<Lsn>, StorageError> {
        let guard = self.fetch_page(PageId(0))?;
        let raw = read_u64(guard.page().data(), header::LAST_CHECKPOINT_LSN_RANGE.start);
        Ok((raw != 0).then_some(Lsn(raw)))
    }

    /// Records `lsn` as the most recent checkpoint's `CheckpointBegin` LSN
    /// in the database header (page 0), on behalf of `txn_id`, so a future
    /// reopen's recovery knows where to start scanning - an ordinary, logged
    /// `PageGuard::write` like `set_catalog_first_page`.
    pub fn set_last_checkpoint_lsn(&self, txn_id: TxnId, lsn: Lsn) -> Result<(), StorageError> {
        let mut guard = self.fetch_page(PageId(0))?;
        guard.write(txn_id, header::LAST_CHECKPOINT_LSN_RANGE.start, &lsn.0.to_le_bytes())
    }

    /// Forward iterator over every durable write-ahead log record with LSN
    /// `>= from`. Used by `storage::recovery` to scan the log.
    pub fn log_iter_from(&self, from: Lsn) -> Result<crate::wal::LogIterator, StorageError> {
        self.log_manager.borrow_mut().iter_from(from)
    }

    /// Reads and decodes exactly one write-ahead log record by its LSN,
    /// without scanning the rest of the log. Used by
    /// `recovery::undo_transaction` to walk a transaction's chain backward
    /// one record at a time.
    pub fn read_log_at(&self, lsn: Lsn) -> Result<Option<crate::wal::LoggedRecord>, StorageError> {
        self.log_manager.borrow_mut().read_at(lsn)
    }

    /// Extends the underlying file so `page_id` is allocated, if it is not
    /// already - i.e. redoes an `AllocPage` record. Idempotent: a no-op if
    /// `page_id` is already within the file's current extent.
    pub fn ensure_page_allocated(&self, page_id: PageId) -> Result<(), StorageError> {
        self.disk_manager.borrow_mut().ensure_allocated(page_id)
    }

    /// Cumulative bytes appended to the write-ahead log since it was
    /// opened, for the checkpoint byte-threshold trigger.
    pub fn log_bytes_appended(&self) -> u64 {
        self.log_manager.borrow().bytes_appended()
    }

    /// `txn_id`'s most recently appended LSN, or `None` if it has never
    /// appended a record. The authoritative way to find where a
    /// transaction's undo chain starts - see `LogManager::last_lsn_for`.
    pub fn last_lsn(&self, txn_id: TxnId) -> Option<Lsn> {
        self.log_manager.borrow().last_lsn_for(txn_id)
    }

    /// The highest transaction id that appears anywhere in the write-ahead
    /// log (excluding `CHECKPOINT_TXN`), or `None` if none has been used
    /// yet. Recovery-only: lets `Database::open` seed the next transaction
    /// id past every id the log has ever used.
    pub fn max_txn_id(&self) -> Option<TxnId> {
        self.log_manager.borrow().max_txn_id()
    }

    /// The `page_lsn` of `page_id`, bringing it into the pool if not
    /// already resident. Recovery-only: lets the Redo pass decide whether a
    /// record needs replaying.
    pub fn page_lsn(&self, page_id: PageId) -> Result<Lsn, StorageError> {
        Ok(self.fetch_page(page_id)?.page().page_lsn())
    }

    /// Recovery-only: applies `bytes` at `offset` on `page_id` and stamps
    /// its `page_lsn` to `lsn`, marking the frame dirty, *without*
    /// appending a new WAL record. Bypasses the ordinary log-then-mutate
    /// discipline `PageGuard::write` enforces, which is correct here only
    /// because the caller is either replaying a record that is already
    /// durably logged (Redo) or has just logged a `Clr` covering exactly
    /// this write itself (Undo) - in both cases logging again would give
    /// the log a false description of what happened.
    pub fn stamp_write(
        &self,
        page_id: PageId,
        offset: usize,
        bytes: &[u8],
        lsn: Lsn,
    ) -> Result<(), StorageError> {
        let guard = self.fetch_page(page_id)?;
        let idx = guard.frame_id.0 as usize;
        // SAFETY: `guard` pins this frame for the duration of this call, so
        // the pool will not evict or reassign it while this borrow is live.
        let page = unsafe { &mut *self.frames[idx].get() };
        page.data_mut()[offset..offset + bytes.len()].copy_from_slice(bytes);
        page.set_page_lsn(lsn);
        if self.dirty_since_lsn[idx].get().is_none() {
            self.dirty_since_lsn[idx].set(Some(lsn));
        }
        Ok(())
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

    /// The only way to mutate a pinned page's bytes: captures the current
    /// bytes at `offset..offset+bytes.len()` as the before-image, appends
    /// an `Update` record on behalf of `txn_id` (before-image for undo,
    /// `bytes` for redo), applies `bytes` to the page, and stamps the
    /// page's `page_lsn` with the record's assigned LSN. Marks the frame
    /// dirty. There is no way to mutate a resident page's bytes that
    /// bypasses this - that is the write-ahead rule enforced at the type
    /// level: no logging, no mutation.
    pub fn write(
        &mut self,
        txn_id: TxnId,
        offset: usize,
        bytes: &[u8],
    ) -> Result<(), StorageError> {
        let idx = self.frame_id.0 as usize;
        // SAFETY: this frame is pinned for the lifetime of this guard, so
        // the pool will never evict or reassign it while this borrow is
        // live.
        let page = unsafe { &mut *self.pool.frames[idx].get() };
        let before = page.data()[offset..offset + bytes.len()].to_vec();
        let lsn = self.pool.append_log(
            txn_id,
            LogRecordKind::Update {
                page_id: self.page_id,
                offset: offset as u16,
                before,
                after: bytes.to_vec(),
            },
        )?;
        page.data_mut()[offset..offset + bytes.len()].copy_from_slice(bytes);
        page.set_page_lsn(lsn);
        if self.pool.dirty_since_lsn[idx].get().is_none() {
            self.pool.dirty_since_lsn[idx].set(Some(lsn));
        }
        Ok(())
    }
}

impl Drop for PageGuard<'_> {
    fn drop(&mut self) {
        self.pool.unpin_frame(self.frame_id);
    }
}

fn read_u32(buf: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([buf[at], buf[at + 1], buf[at + 2], buf[at + 3]])
}

fn read_u64(buf: &[u8], at: usize) -> u64 {
    u64::from_le_bytes([
        buf[at],
        buf[at + 1],
        buf[at + 2],
        buf[at + 3],
        buf[at + 4],
        buf[at + 5],
        buf[at + 6],
        buf[at + 7],
    ])
}
