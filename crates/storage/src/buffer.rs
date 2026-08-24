use std::cell::{Cell, RefCell, UnsafeCell};
use std::collections::HashMap;

use common::{FrameId, Lsn, PageId, TxnId};

use crate::disk::{DiskManager, header};
use crate::dwb::DoubleWriteBuffer;
use crate::error::StorageError;
use crate::page::{Page, PageGuard};
use crate::replacer::Replacer;
use crate::wal::{LogManager, LogRecord, LogRecordKind};

pub struct BufferPool {
    disk_manager: RefCell<DiskManager>,
    dwb: RefCell<DoubleWriteBuffer>,
    log_manager: RefCell<LogManager>,
    frames: Vec<UnsafeCell<Page>>,
    page_table: RefCell<HashMap<PageId, FrameId>>,
    frame_page: Vec<Cell<Option<PageId>>>,
    pin_counts: Vec<Cell<usize>>,
    dirty_since_lsn: Vec<Cell<Option<Lsn>>>,
    free_list: RefCell<Vec<FrameId>>,
    replacer: RefCell<Box<dyn Replacer>>,
    #[cfg(any(test, feature = "test-util"))]
    fetch_count: Cell<usize>,
    #[cfg(any(test, feature = "test-util"))]
    write_observations: RefCell<Vec<WriteObservation>>,
}

#[cfg(any(test, feature = "test-util"))]
#[derive(Debug, Clone, Copy)]
pub struct WriteObservation {
    pub page_id: PageId,
    pub page_lsn: Lsn,
    pub durable_lsn: Lsn,
}

impl BufferPool {
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

    #[cfg(any(test, feature = "test-util"))]
    pub fn fetch_count(&self) -> usize {
        self.fetch_count.get()
    }

    #[cfg(any(test, feature = "test-util"))]
    pub fn reset_fetch_count(&self) {
        self.fetch_count.set(0);
    }

    #[cfg(any(test, feature = "test-util"))]
    pub fn write_observations(&self) -> Vec<WriteObservation> {
        self.write_observations.borrow().clone()
    }

    pub fn fetch_page(&self, page_id: PageId) -> Result<PageGuard<'_>, StorageError> {
        tracing::trace!(page_id = page_id.0, "fetch_page");
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

    pub fn new_page(&self, txn_id: TxnId) -> Result<(PageId, PageGuard<'_>), StorageError> {
        let page_id = self.disk_manager.borrow_mut().allocate_page()?;
        let lsn = self.append_log(txn_id, LogRecordKind::AllocPage { page_id })?;
        let frame_id = self.allocate_frame()?;
        self.install(frame_id, page_id, Page::new(page_id), Some(lsn));
        self.pin(frame_id);
        Ok((page_id, PageGuard { page_id, frame_id, pool: self }))
    }

    pub fn flush_page(&self, page_id: PageId) -> Result<(), StorageError> {
        let frame_id = self.frame_of(page_id)?;
        if self.dirty_since_lsn[frame_id.0 as usize].get().is_some() {
            self.flush_pages(&[(frame_id, page_id)])?;
        }
        Ok(())
    }

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

        let capacity = self.dwb.borrow().capacity();
        for batch in dirty.chunks(capacity) {
            self.flush_pages(batch)?;
        }
        Ok(())
    }

    pub fn sync(&self) -> Result<(), StorageError> {
        self.disk_manager.borrow_mut().sync()
    }

    pub fn catalog_first_page(&self) -> Result<Option<PageId>, StorageError> {
        let guard = self.fetch_page(PageId(0))?;
        let raw = read_u32(guard.page().data(), header::CATALOG_FIRST_PAGE_RANGE.start);
        Ok((raw != u32::MAX).then_some(PageId(raw)))
    }

    pub fn set_catalog_first_page(
        &self,
        txn_id: TxnId,
        page_id: PageId,
    ) -> Result<(), StorageError> {
        let mut guard = self.fetch_page(PageId(0))?;
        guard.write(txn_id, header::CATALOG_FIRST_PAGE_RANGE.start, &page_id.0.to_le_bytes())
    }

    pub fn append_log(&self, txn_id: TxnId, kind: LogRecordKind) -> Result<Lsn, StorageError> {
        self.log_manager.borrow_mut().append(LogRecord { txn_id, kind })
    }

    pub fn flush_log(&self, up_to: Lsn) -> Result<(), StorageError> {
        self.log_manager.borrow_mut().flush(up_to)
    }

    pub fn flush_log_all(&self) -> Result<(), StorageError> {
        self.log_manager.borrow_mut().flush_all()
    }

    pub fn durable_lsn(&self) -> Lsn {
        self.log_manager.borrow().durable_lsn()
    }

    fn allocate_frame(&self) -> Result<FrameId, StorageError> {
        if let Some(frame_id) = self.free_list.borrow_mut().pop() {
            return Ok(frame_id);
        }

        let Some(frame_id) = self.replacer.borrow_mut().evict() else {
            tracing::warn!(
                pool_size = self.frames.len(),
                "buffer pool exhausted: every frame is pinned"
            );
            return Err(StorageError::BufferPoolExhausted);
        };

        let idx = frame_id.0 as usize;
        if let Some(victim_page_id) = self.frame_page[idx].get() {
            tracing::trace!(page_id = victim_page_id.0, frame_id = frame_id.0, "evict");
            if self.dirty_since_lsn[idx].get().is_some() {
                self.flush_pages(&[(frame_id, victim_page_id)])?;
            }
            self.page_table.borrow_mut().remove(&victim_page_id);
            self.frame_page[idx].set(None);
        }
        Ok(frame_id)
    }

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

    fn flush_pages(&self, pages: &[(FrameId, PageId)]) -> Result<(), StorageError> {
        if pages.is_empty() {
            return Ok(());
        }
        tracing::trace!(pages = pages.len(), "flush_pages");
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

    pub fn dirty_page_table(&self) -> Vec<(PageId, Lsn)> {
        self.frame_page
            .iter()
            .zip(self.dirty_since_lsn.iter())
            .filter_map(|(page_id, dirty_lsn)| Some((page_id.get()?, dirty_lsn.get()?)))
            .collect()
    }

    pub fn last_checkpoint_lsn(&self) -> Result<Option<Lsn>, StorageError> {
        let guard = self.fetch_page(PageId(0))?;
        let raw = read_u64(guard.page().data(), header::LAST_CHECKPOINT_LSN_RANGE.start);
        Ok((raw != 0).then_some(Lsn(raw)))
    }

    pub fn set_last_checkpoint_lsn(&self, txn_id: TxnId, lsn: Lsn) -> Result<(), StorageError> {
        let mut guard = self.fetch_page(PageId(0))?;
        guard.write(txn_id, header::LAST_CHECKPOINT_LSN_RANGE.start, &lsn.0.to_le_bytes())
    }

    pub fn log_iter_from(&self, from: Lsn) -> Result<crate::wal::LogIterator, StorageError> {
        self.log_manager.borrow_mut().iter_from(from)
    }

    pub fn read_log_at(&self, lsn: Lsn) -> Result<Option<crate::wal::LoggedRecord>, StorageError> {
        self.log_manager.borrow_mut().read_at(lsn)
    }

    pub fn ensure_page_allocated(&self, page_id: PageId) -> Result<(), StorageError> {
        self.disk_manager.borrow_mut().ensure_allocated(page_id)
    }

    pub fn log_bytes_appended(&self) -> u64 {
        self.log_manager.borrow().bytes_appended()
    }

    pub fn last_lsn(&self, txn_id: TxnId) -> Option<Lsn> {
        self.log_manager.borrow().last_lsn_for(txn_id)
    }

    pub fn max_txn_id(&self) -> Option<TxnId> {
        self.log_manager.borrow().max_txn_id()
    }

    pub fn page_lsn(&self, page_id: PageId) -> Result<Lsn, StorageError> {
        Ok(self.fetch_page(page_id)?.page().page_lsn())
    }

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
    pub fn page(&self) -> &Page {
        let idx = self.frame_id.0 as usize;
        // SAFETY: this guard's existence means the frame is pinned, so the
        // pool will not reassign or evict it out from under this borrow.
        unsafe { &*self.pool.frames[idx].get() }
    }

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
