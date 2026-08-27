use std::collections::HashMap;
use std::mem::ManuallyDrop;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Mutex, RwLock};

use common::sync::recover_lock;
use common::{FrameId, Lsn, PageId, TxnId};

use crate::disk::{DiskManager, header};
use crate::dwb::DoubleWriteBuffer;
use crate::error::StorageError;
use crate::page::{Page, PageReadGuard, PageWriteGuard};
use crate::replacer::Replacer;
use crate::wal::{LogManager, LogRecord, LogRecordKind};

#[cfg(any(test, feature = "test-util"))]
use std::sync::atomic::AtomicUsize;

struct Frame {
    page: RwLock<Page>,
    pin_count: AtomicU32,
    dirty_since_lsn: AtomicU64,
}

struct PoolIndex {
    page_table: HashMap<PageId, FrameId>,
    frame_page: Vec<Option<PageId>>,
    free_list: Vec<FrameId>,
    replacer: Box<dyn Replacer>,
}

pub struct BufferPool {
    disk_manager: DiskManager,
    dwb: DoubleWriteBuffer,
    log_manager: LogManager,
    frames: Box<[Frame]>,
    index: Mutex<PoolIndex>,
    #[cfg(any(test, feature = "test-util"))]
    fetch_count: AtomicUsize,
    #[cfg(any(test, feature = "test-util"))]
    write_observations: Mutex<Vec<WriteObservation>>,
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
        let frames: Box<[Frame]> = (0..pool_size)
            .map(|_| Frame {
                page: RwLock::new(Page::new(PageId(0))),
                pin_count: AtomicU32::new(0),
                dirty_since_lsn: AtomicU64::new(0),
            })
            .collect();
        let frame_page = vec![None; pool_size];
        let free_list = (0..pool_size as u32).map(FrameId).collect();

        Self {
            disk_manager,
            dwb,
            log_manager,
            frames,
            index: Mutex::new(PoolIndex {
                page_table: HashMap::new(),
                frame_page,
                free_list,
                replacer,
            }),
            #[cfg(any(test, feature = "test-util"))]
            fetch_count: AtomicUsize::new(0),
            #[cfg(any(test, feature = "test-util"))]
            write_observations: Mutex::new(Vec::new()),
        }
    }

    #[cfg(any(test, feature = "test-util"))]
    pub fn fetch_count(&self) -> usize {
        self.fetch_count.load(Ordering::Relaxed)
    }

    #[cfg(any(test, feature = "test-util"))]
    pub fn reset_fetch_count(&self) {
        self.fetch_count.store(0, Ordering::Relaxed);
    }

    #[cfg(any(test, feature = "test-util"))]
    pub fn write_observations(&self) -> Vec<WriteObservation> {
        recover_lock(self.write_observations.lock(), "BufferPool.write_observations").clone()
    }

    pub fn fetch_page(&self, page_id: PageId) -> Result<PageWriteGuard<'_>, StorageError> {
        let frame_id = self.fetch_frame(page_id)?;
        Ok(self.write_guard(page_id, frame_id))
    }

    pub fn fetch_page_read(&self, page_id: PageId) -> Result<PageReadGuard<'_>, StorageError> {
        let frame_id = self.fetch_frame(page_id)?;
        Ok(self.read_guard(page_id, frame_id))
    }

    fn fetch_frame(&self, page_id: PageId) -> Result<FrameId, StorageError> {
        tracing::trace!(page_id = page_id.0, "fetch_page");
        #[cfg(any(test, feature = "test-util"))]
        self.fetch_count.fetch_add(1, Ordering::Relaxed);

        if let Some(frame_id) = self.pin_if_cached(page_id) {
            metrics::counter!("buffer_pool_hits_total").increment(1);
            return Ok(frame_id);
        }

        metrics::counter!("buffer_pool_misses_total").increment(1);
        let frame_id = self.acquire_free_frame()?;
        let mut page = Page::new(page_id);
        self.disk_manager.read_page(page_id, &mut page)?;
        self.install(frame_id, page_id, page, None);
        Ok(frame_id)
    }

    pub fn new_page(&self, txn_id: TxnId) -> Result<(PageId, PageWriteGuard<'_>), StorageError> {
        let page_id = self.disk_manager.allocate_page()?;
        let lsn = self.append_log(txn_id, LogRecordKind::AllocPage { page_id })?;
        let frame_id = self.acquire_free_frame()?;
        self.install(frame_id, page_id, Page::new(page_id), Some(lsn));
        Ok((page_id, self.write_guard(page_id, frame_id)))
    }

    pub fn flush_page(&self, page_id: PageId) -> Result<(), StorageError> {
        let frame_id = self.frame_of(page_id)?;
        if self.frames[frame_id.0 as usize].dirty_since_lsn.load(Ordering::Acquire) != 0 {
            self.flush_pages(&[(frame_id, page_id)])?;
        }
        Ok(())
    }

    pub fn flush_all(&self) -> Result<(), StorageError> {
        let dirty: Vec<(FrameId, PageId)> = {
            let index = recover_lock(self.index.lock(), "BufferPool.index");
            index
                .frame_page
                .iter()
                .enumerate()
                .filter_map(|(idx, page_id)| {
                    let page_id = (*page_id)?;
                    (self.frames[idx].dirty_since_lsn.load(Ordering::Acquire) != 0)
                        .then_some((FrameId(idx as u32), page_id))
                })
                .collect()
        };

        let capacity = self.dwb.capacity();
        for batch in dirty.chunks(capacity) {
            self.flush_pages(batch)?;
        }
        Ok(())
    }

    pub fn sync(&self) -> Result<(), StorageError> {
        self.disk_manager.sync()
    }

    pub fn catalog_first_page(&self) -> Result<Option<PageId>, StorageError> {
        let guard = self.fetch_page_read(PageId(0))?;
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

    pub fn index_catalog_first_page(&self) -> Result<Option<PageId>, StorageError> {
        let guard = self.fetch_page_read(PageId(0))?;
        let raw = read_u32(guard.page().data(), header::INDEX_CATALOG_FIRST_PAGE_RANGE.start);
        Ok((raw != u32::MAX).then_some(PageId(raw)))
    }

    pub fn set_index_catalog_first_page(
        &self,
        txn_id: TxnId,
        page_id: PageId,
    ) -> Result<(), StorageError> {
        let mut guard = self.fetch_page(PageId(0))?;
        guard.write(txn_id, header::INDEX_CATALOG_FIRST_PAGE_RANGE.start, &page_id.0.to_le_bytes())
    }

    pub fn append_log(&self, txn_id: TxnId, kind: LogRecordKind) -> Result<Lsn, StorageError> {
        self.log_manager.append(LogRecord { txn_id, kind })
    }

    pub fn flush_log(&self, up_to: Lsn) -> Result<(), StorageError> {
        self.log_manager.flush(up_to)
    }

    pub fn flush_log_all(&self) -> Result<(), StorageError> {
        self.log_manager.flush_all()
    }

    pub fn durable_lsn(&self) -> Lsn {
        self.log_manager.durable_lsn()
    }

    fn pin_if_cached(&self, page_id: PageId) -> Option<FrameId> {
        let mut index = recover_lock(self.index.lock(), "BufferPool.index");
        let frame_id = *index.page_table.get(&page_id)?;
        self.pin_locked(&mut index, frame_id);
        Some(frame_id)
    }

    fn pin_locked(&self, index: &mut PoolIndex, frame_id: FrameId) {
        self.frames[frame_id.0 as usize].pin_count.fetch_add(1, Ordering::AcqRel);
        metrics::gauge!("buffer_pool_pinned_frames").increment(1.0);
        index.replacer.record_access(frame_id);
        index.replacer.set_evictable(frame_id, false);
    }

    fn acquire_free_frame(&self) -> Result<FrameId, StorageError> {
        let (frame_id, victim_page_id) = {
            let mut index = recover_lock(self.index.lock(), "BufferPool.index");
            if let Some(frame_id) = index.free_list.pop() {
                (frame_id, None)
            } else {
                let Some(frame_id) = index.replacer.evict() else {
                    tracing::warn!(
                        pool_size = self.frames.len(),
                        "buffer pool exhausted: every frame is pinned"
                    );
                    return Err(StorageError::BufferPoolExhausted);
                };
                let idx = frame_id.0 as usize;
                let victim_page_id = index.frame_page[idx].take();
                if let Some(victim_page_id) = victim_page_id {
                    index.page_table.remove(&victim_page_id);
                }
                (frame_id, victim_page_id)
            }
        };

        if let Some(victim_page_id) = victim_page_id {
            tracing::trace!(page_id = victim_page_id.0, frame_id = frame_id.0, "evict");
            metrics::counter!("buffer_pool_evictions_total").increment(1);
            if self.frames[frame_id.0 as usize].dirty_since_lsn.load(Ordering::Acquire) != 0 {
                self.flush_pages(&[(frame_id, victim_page_id)])?;
            }
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
        *recover_lock(self.frames[idx].page.write(), "BufferPool.frame.page") = page;
        self.frames[idx]
            .dirty_since_lsn
            .store(dirty_since_lsn.map_or(0, |lsn| lsn.0), Ordering::Release);

        let mut index = recover_lock(self.index.lock(), "BufferPool.index");
        index.page_table.insert(page_id, frame_id);
        index.frame_page[idx] = Some(page_id);
        self.pin_locked(&mut index, frame_id);
    }

    fn write_guard(&self, page_id: PageId, frame_id: FrameId) -> PageWriteGuard<'_> {
        let bytes =
            recover_lock(self.frames[frame_id.0 as usize].page.write(), "BufferPool.frame.page");
        PageWriteGuard { page_id, frame_id, bytes: ManuallyDrop::new(bytes), pool: self }
    }

    fn read_guard(&self, page_id: PageId, frame_id: FrameId) -> PageReadGuard<'_> {
        let bytes =
            recover_lock(self.frames[frame_id.0 as usize].page.read(), "BufferPool.frame.page");
        PageReadGuard { page_id, frame_id, bytes: ManuallyDrop::new(bytes), pool: self }
    }

    fn unpin_frame(&self, frame_id: FrameId) {
        let idx = frame_id.0 as usize;
        let pin_count = &self.frames[idx].pin_count;
        let current = pin_count.load(Ordering::Acquire);
        debug_assert!(current > 0, "pin count underflow on frame {frame_id:?}");
        if current == 0 {
            return;
        }
        let remaining = pin_count.fetch_sub(1, Ordering::AcqRel) - 1;
        metrics::gauge!("buffer_pool_pinned_frames").decrement(1.0);
        if remaining == 0 {
            recover_lock(self.index.lock(), "BufferPool.index")
                .replacer
                .set_evictable(frame_id, true);
        }
    }

    fn flush_pages(&self, pages: &[(FrameId, PageId)]) -> Result<(), StorageError> {
        if pages.is_empty() {
            return Ok(());
        }
        tracing::trace!(pages = pages.len(), "flush_pages");
        debug_assert!(
            pages.len() <= self.dwb.capacity(),
            "flush batch of {} pages exceeds the double-write buffer's capacity",
            pages.len()
        );

        let snapshot: Vec<Page> = pages
            .iter()
            .map(|&(frame_id, _)| {
                recover_lock(self.frames[frame_id.0 as usize].page.read(), "BufferPool.frame.page")
                    .clone()
            })
            .collect();

        let max_page_lsn = snapshot.iter().map(Page::page_lsn).max().unwrap_or(Lsn(0));
        self.log_manager.flush(max_page_lsn)?;
        let durable_lsn = self.log_manager.durable_lsn();
        debug_assert!(
            durable_lsn >= max_page_lsn,
            "batch reached disk with max page_lsn {max_page_lsn:?} ahead of durable_lsn \
             {durable_lsn:?}"
        );
        #[cfg(any(test, feature = "test-util"))]
        {
            let mut observations =
                recover_lock(self.write_observations.lock(), "BufferPool.write_observations");
            for page in &snapshot {
                observations.push(WriteObservation {
                    page_id: page.id(),
                    page_lsn: page.page_lsn(),
                    durable_lsn,
                });
            }
        }

        self.dwb.write_batch(&snapshot)?;
        metrics::counter!("dwb_batches_written_total").increment(1);

        for page in &snapshot {
            self.disk_manager.write_page(page.id(), page)?;
        }
        self.disk_manager.sync()?;

        self.dwb.clear_batch()?;

        for &(frame_id, _) in pages {
            self.frames[frame_id.0 as usize].dirty_since_lsn.store(0, Ordering::Release);
        }
        Ok(())
    }

    fn frame_of(&self, page_id: PageId) -> Result<FrameId, StorageError> {
        recover_lock(self.index.lock(), "BufferPool.index")
            .page_table
            .get(&page_id)
            .copied()
            .ok_or(StorageError::PageNotFound(page_id.0))
    }

    pub fn dirty_page_table(&self) -> Vec<(PageId, Lsn)> {
        let index = recover_lock(self.index.lock(), "BufferPool.index");
        index
            .frame_page
            .iter()
            .enumerate()
            .filter_map(|(idx, page_id)| {
                let page_id = (*page_id)?;
                let dirty_since_lsn = self.frames[idx].dirty_since_lsn.load(Ordering::Acquire);
                (dirty_since_lsn != 0).then_some((page_id, Lsn(dirty_since_lsn)))
            })
            .collect()
    }

    pub fn last_checkpoint_lsn(&self) -> Result<Option<Lsn>, StorageError> {
        let guard = self.fetch_page_read(PageId(0))?;
        let raw = read_u64(guard.page().data(), header::LAST_CHECKPOINT_LSN_RANGE.start);
        Ok((raw != 0).then_some(Lsn(raw)))
    }

    pub fn set_last_checkpoint_lsn(&self, txn_id: TxnId, lsn: Lsn) -> Result<(), StorageError> {
        let mut guard = self.fetch_page(PageId(0))?;
        guard.write(txn_id, header::LAST_CHECKPOINT_LSN_RANGE.start, &lsn.0.to_le_bytes())
    }

    pub fn log_iter_from(&self, from: Lsn) -> Result<crate::wal::LogIterator, StorageError> {
        self.log_manager.iter_from(from)
    }

    pub fn read_log_at(&self, lsn: Lsn) -> Result<Option<crate::wal::LoggedRecord>, StorageError> {
        self.log_manager.read_at(lsn)
    }

    pub fn ensure_page_allocated(&self, page_id: PageId) -> Result<(), StorageError> {
        self.disk_manager.ensure_allocated(page_id)
    }

    pub fn log_bytes_appended(&self) -> u64 {
        self.log_manager.bytes_appended()
    }

    pub fn last_lsn(&self, txn_id: TxnId) -> Option<Lsn> {
        self.log_manager.last_lsn_for(txn_id)
    }

    pub fn max_txn_id(&self) -> Option<TxnId> {
        self.log_manager.max_txn_id()
    }

    pub fn page_lsn(&self, page_id: PageId) -> Result<Lsn, StorageError> {
        Ok(self.fetch_page_read(page_id)?.page().page_lsn())
    }

    pub fn stamp_write(
        &self,
        page_id: PageId,
        offset: usize,
        bytes: &[u8],
        lsn: Lsn,
    ) -> Result<(), StorageError> {
        let mut guard = self.fetch_page(page_id)?;
        let idx = guard.frame_id.0 as usize;
        guard.bytes.data_mut()[offset..offset + bytes.len()].copy_from_slice(bytes);
        guard.bytes.set_page_lsn(lsn);
        if self.frames[idx].dirty_since_lsn.load(Ordering::Acquire) == 0 {
            self.frames[idx].dirty_since_lsn.store(lsn.0, Ordering::Release);
        }
        Ok(())
    }
}

impl PageReadGuard<'_> {
    pub fn page(&self) -> &Page {
        &self.bytes
    }
}

impl Drop for PageReadGuard<'_> {
    fn drop(&mut self) {
        // SAFETY: `bytes` is taken exactly once, here, and never accessed again -
        // dropping it first releases the frame's latch before `unpin_frame` may
        // let the pool reassign or evict the frame.
        unsafe { ManuallyDrop::drop(&mut self.bytes) };
        self.pool.unpin_frame(self.frame_id);
    }
}

impl PageWriteGuard<'_> {
    pub fn page(&self) -> &Page {
        &self.bytes
    }

    pub fn write(
        &mut self,
        txn_id: TxnId,
        offset: usize,
        bytes: &[u8],
    ) -> Result<(), StorageError> {
        let idx = self.frame_id.0 as usize;
        let before = self.bytes.data()[offset..offset + bytes.len()].to_vec();
        let lsn = self.pool.append_log(
            txn_id,
            LogRecordKind::Update {
                page_id: self.page_id,
                offset: offset as u16,
                before,
                after: bytes.to_vec(),
            },
        )?;
        self.bytes.data_mut()[offset..offset + bytes.len()].copy_from_slice(bytes);
        self.bytes.set_page_lsn(lsn);
        if self.pool.frames[idx].dirty_since_lsn.load(Ordering::Acquire) == 0 {
            self.pool.frames[idx].dirty_since_lsn.store(lsn.0, Ordering::Release);
        }
        Ok(())
    }
}

impl Drop for PageWriteGuard<'_> {
    fn drop(&mut self) {
        // SAFETY: same as `PageReadGuard`'s Drop - `bytes` is taken exactly once
        // and dropped before the pin is released.
        unsafe { ManuallyDrop::drop(&mut self.bytes) };
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
