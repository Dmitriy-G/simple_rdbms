# buffer

`BufferPool`, which caches a bounded number of pages in memory, mediating
every access to pages between the disk manager and the layers above (heap
files, B+tree, WAL). A page is pinned for as long as a caller holds a
`PageGuard` to it; once unpinned it becomes a candidate for eviction,
chosen by the configured `Replacer`, when a new page must be loaded and
no frame is free. `page()`, `write()`, and `Drop` for `PageGuard` (defined
in `page.rs`) live here instead, since they need access to `BufferPool`'s
private frame table, pin counts, dirty flags, and log manager.

Every field on `BufferPool` is behind interior mutability
(`RefCell`/`Cell`/`UnsafeCell`) so that `fetch_page`/`new_page` can take
`&self` rather than `&mut self`: a `PageGuard`'s whole point is to let
multiple pages be pinned at once, which isn't possible if borrowing one
page ties up an exclusive borrow of the whole pool. `disk_manager`
serializes the (rare) actual disk I/O calls; `dwb` is the double-write
buffer every dirty page is copied into (and that copy synced) before the
page itself reaches its real location (see `flush_pages`); `log_manager`
is the write-ahead log every page mutation is recorded into before it
reaches this pool's own bytes (see `PageGuard::write` and `flush_pages`);
`page_table`/`frame_page` are the two-way `PageId <-> FrameId` mapping;
`pin_counts`/`dirty_since_lsn` are per-frame `Cell`s, one independent lock
each; `replacer` is the eviction policy's own bookkeeping; `frames` is an
`UnsafeCell` per frame, whose safety relies on the pin-count protocol
above - a frame is only ever mutated by code that holds it pinned (a live
`PageGuard`, or the pool itself while the frame is unpinned and thus
inaccessible to any guard), so two overlapping `&mut` borrows of the same
frame's bytes never occur. This is the invariant every `// SAFETY:`
comment in this file cites.

## Key Components

- `BufferPool` - see above for the field-by-field breakdown.
- `WriteObservation` - test/`test-util`-only. One observed disk write, for
  the WAL-ordering invariant test: at the moment `page_id` was written to
  disk, its `page_lsn` and the log's `durable_lsn` at that instant. The
  write-ahead rule requires `durable_lsn >= page_lsn` for every entry.
- `BufferPool::new(disk_manager, dwb, log_manager, pool_size, replacer)` -
  creates a buffer pool of `pool_size` frames, using `replacer` to choose
  eviction victims, logging every page mutation through `log_manager`
  before it is applied, and protecting every flush against a torn write
  through `dwb` (see `flush_pages`).
- `BufferPool::fetch_count`/`reset_fetch_count`/`write_observations` -
  test/`test-util`-only introspection: the number of `fetch_page` calls
  made so far (for tests asserting an operation touched only the pages it
  needed), and every disk write `flush_frame` has performed, in order.
- `BufferPool::fetch_page(&self, page_id) -> Result<PageGuard<'_>, StorageError>` -
  fetches `page_id`, pinning it and returning a guard. If the page is not
  already resident, brings it in from disk, evicting a victim frame via
  the replacer if none is free.
- `BufferPool::new_page(&self, txn_id) -> Result<(PageId, PageGuard<'_>), StorageError>` -
  allocates a brand-new page via the disk manager and pins it in the
  pool, returning a guard to it. Logs an `AllocPage` record on behalf of
  `txn_id` before installing the page.
- `BufferPool::flush_page(&self, page_id)` - flushes `page_id` to disk if
  its frame is dirty.
- `BufferPool::flush_all(&self)` - flushes every dirty resident page to
  disk, batching them through the double-write buffer in groups of at
  most `dwb.capacity()` pages - one double-write round trip per group
  rather than per page, which is where batching actually pays for the
  doubled writes `flush_pages` costs. `DoubleWriteBuffer::open` rejects
  `capacity == 0`, so the chunk size here is always positive.
- `BufferPool::sync(&self)` - forces every write made so far to durable
  storage. Callers should `flush_all` first; this only syncs whatever has
  already reached the disk manager.
- `BufferPool::catalog_first_page(&self) -> Result<Option<PageId>, StorageError>` -
  the catalog's first page, as recorded in the database header (page 0),
  or `None` if the catalog has not been persisted yet. Reads page 0
  through the ordinary buffer-pool path - fetching it like any other page
  - rather than a value cached before crash recovery ran, so this always
  reflects the current, possibly just-redone-or-undone state.
- `BufferPool::set_catalog_first_page(&self, txn_id, page_id)` - records
  `page_id` as the catalog's first page in the database header, on
  behalf of `txn_id` - an ordinary, logged `PageGuard::write` like any
  other page mutation, so it is redone or undone exactly like the catalog
  heap allocation it accompanies rather than racing ahead of it as an
  unlogged direct write.
- `BufferPool::append_log(&self, txn_id, kind) -> Result<Lsn, StorageError>` -
  appends `kind` to the write-ahead log on behalf of `txn_id`, returning
  its assigned LSN. Does not by itself make the record durable; call
  `flush_log`/`flush_log_all` for that.
- `BufferPool::flush_log(&self, up_to)`/`flush_log_all(&self)` - forces
  log records up to `up_to`, or every record appended so far, to disk.
- `BufferPool::durable_lsn(&self) -> Lsn` - the write-ahead log's current
  durable LSN.
- `BufferPool::allocate_frame(&self) -> Result<FrameId, StorageError>` -
  private. Finds a frame to hold a newly-fetched or newly-allocated page:
  reuses a never-used frame if one is free, otherwise asks the replacer
  for an evictable victim, flushing it first if dirty.
- `BufferPool::install(&self, frame_id, page_id, page, dirty_since_lsn)` -
  private. Installs `page` into `frame_id`'s slot and records it under
  `page_id` in the page table. Only ever called on a frame that was just
  freed or evicted, so no live guard can be observing its bytes.
  `dirty_since_lsn` is `Some(lsn)` if the page is already dirty as of
  installation (a brand-new page, dirtied by its own `AllocPage` record),
  or `None` for a page freshly read from disk.
- `BufferPool::pin(&self, frame_id)`/`unpin_frame(&self, frame_id)` -
  private. Increment/decrement a frame's pin count and update the
  replacer's evictability accordingly.
- `BufferPool::flush_pages(&self, pages) -> Result<(), StorageError>` -
  private. Flushes a batch of currently-dirty, resident pages to disk
  through the double-write buffer. Every entry in `pages` must already be
  dirty and safe from concurrent reassignment; this does not re-check
  either condition. Implements the write-ahead rule (the log must be
  durable to the batch's *highest* `page_lsn` before anything reaches the
  double-write buffer - not before each page individually, and not
  merely before the real file, which is what made a torn write
  unrecoverable before the double-write buffer existed) and the
  double-write protocol itself: write every page's image to `dwb` and
  sync it (so the batch is now recoverable even if nothing below has
  happened yet), only then write every page to its real location and
  sync that, then clear `dwb`. A crash at any point along the way leaves
  either a real page untouched by this batch, or a torn real page with
  an intact `dwb` copy `recovery::recover_double_write` can restore from
  - never a torn real page with no way back.
- `BufferPool::frame_of(&self, page_id) -> Result<FrameId, StorageError>` -
  private.
- `BufferPool::dirty_page_table(&self) -> Vec<(PageId, Lsn)>` - the Dirty
  Page Table: every currently-dirty resident page and its
  `recovery_lsn` (the LSN of the record that first dirtied it since its
  last flush). A checkpoint snapshot of this is what lets recovery's
  Redo pass start from the oldest change that might still be missing
  from disk, rather than the beginning of the log.
- `BufferPool::last_checkpoint_lsn(&self)`/`set_last_checkpoint_lsn(&self, txn_id, lsn)` -
  read/write the most recent checkpoint's `CheckpointBegin` LSN in the
  database header, through the same logged `PageGuard::write` path as
  `catalog_first_page`.
- `BufferPool::log_iter_from(&self, from) -> Result<LogIterator, StorageError>` -
  forward iterator over every durable write-ahead log record with LSN
  `>= from`. Used by `storage::recovery` to scan the log.
- `BufferPool::read_log_at(&self, lsn) -> Result<Option<LoggedRecord>, StorageError>` -
  reads and decodes exactly one write-ahead log record by its LSN,
  without scanning the rest of the log. Used by `recovery::undo_transaction`
  to walk a transaction's chain backward one record at a time.
- `BufferPool::ensure_page_allocated(&self, page_id)` - extends the
  underlying file so `page_id` is allocated, if it is not already - i.e.
  redoes an `AllocPage` record. Idempotent.
- `BufferPool::log_bytes_appended(&self) -> u64` - cumulative bytes
  appended to the write-ahead log since it was opened, for the checkpoint
  byte-threshold trigger.
- `BufferPool::last_lsn(&self, txn_id) -> Option<Lsn>` - `txn_id`'s most
  recently appended LSN, or `None` if it has never appended a record. The
  authoritative way to find where a transaction's undo chain starts.
- `BufferPool::max_txn_id(&self) -> Option<TxnId>` - the highest
  transaction id that appears anywhere in the write-ahead log (excluding
  `CHECKPOINT_TXN`), or `None` if none has been used yet. Recovery-only:
  lets `Database::open` seed the next transaction id past every id the
  log has ever used.
- `BufferPool::page_lsn(&self, page_id) -> Result<Lsn, StorageError>` -
  the `page_lsn` of `page_id`, bringing it into the pool if not already
  resident. Recovery-only: lets the Redo pass decide whether a record
  needs replaying.
- `BufferPool::stamp_write(&self, page_id, offset, bytes, lsn)` -
  recovery-only: applies `bytes` at `offset` on `page_id` and stamps its
  `page_lsn` to `lsn`, marking the frame dirty, *without* appending a new
  WAL record. Bypasses the ordinary log-then-mutate discipline
  `PageGuard::write` enforces, which is correct here only because the
  caller is either replaying a record that is already durably logged
  (Redo) or has just logged a `Clr` covering exactly this write itself
  (Undo) - in both cases logging again would give the log a false
  description of what happened.
- `PageGuard::page(&self) -> &Page` - read-only access to the pinned
  page.
- `PageGuard::write(&mut self, txn_id, offset, bytes)` - the only way to
  mutate a pinned page's bytes: captures the current bytes at
  `offset..offset+bytes.len()` as the before-image, appends an `Update`
  record on behalf of `txn_id` (before-image for undo, `bytes` for redo),
  applies `bytes` to the page, and stamps the page's `page_lsn` with the
  record's assigned LSN. Marks the frame dirty. There is no way to mutate
  a resident page's bytes that bypasses this - the write-ahead rule
  enforced at the type level: no logging, no mutation.
- `impl Drop for PageGuard` - unpins the frame.
- `read_u32`/`read_u64` - private helpers.

## Usage Example

```rust
use storage::buffer::BufferPool;

let pool = BufferPool::new(disk, dwb, log, 64, Box::new(replacer));
let (page_id, mut guard) = pool.new_page(txn_id)?;
guard.write(txn_id, 20, b"hello")?;
drop(guard);
pool.flush_all()?;
```

`heap::SlottedPage`/`TableHeap` are `BufferPool`'s main callers above
`storage`: every tuple insert, delete, and scan goes through
`fetch_page`/`new_page` to get a `PageGuard`, and `PageGuard::write` to
mutate it. `recovery::recover` drives the pool directly during Redo/Undo
via `stamp_write`/`page_lsn`/`log_iter_from`/`read_log_at`. `engine::Database`
calls `flush_all`/`sync`/`flush_log_all` at clean shutdown.
