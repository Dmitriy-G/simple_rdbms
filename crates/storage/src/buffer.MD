# buffer

`BufferPool`, which caches a bounded number of pages in memory, mediating
every access to pages between the disk manager and the layers above (heap
files, B+tree, WAL). A page is pinned for as long as a caller holds a
`PageReadGuard`/`PageWriteGuard` to it; once unpinned it becomes a
candidate for eviction, chosen by the configured `Replacer`, when a new
page must be loaded and no frame is free. `page()`, `write()`, and `Drop`
for both guards (defined in `page.rs`) live here instead, since they need
access to `BufferPool`'s private frame table and log manager.

`BufferPool` is `Send + Sync` (M10.1, `docs/ROADMAP.md`) so that a future
connection layer can share one pool across threads; execution itself
stays serial through M10.1 - only the type-level barrier to concurrency
is gone. The state is split by what it guards, not merged into one big
lock, so that a page's own bytes are never blocked on bookkeeping for a
*different* page:

1. **`Mutex<PoolIndex>`** (`index`) - the page table (`PageId -> FrameId`),
   its reverse mapping (`frame_page`), the free list, and the replacer,
   held together in one struct since every mutation to one of them
   (installing a page, evicting a victim) always needs to update the
   others atomically with it. Held only for lookups and frame
   bookkeeping - **never** across a frame latch acquisition or disk I/O.
2. **Per frame** (`Frame`, private) - `page: RwLock<Page>` for the page's
   own bytes (readers and a lone writer coexist per the RwLock's usual
   rules; see `PageReadGuard`/`PageWriteGuard` below), `pin_count:
   AtomicU32`, and `dirty_since_lsn: AtomicU64` (`0` means clean; a
   nonzero value is the LSN of the record that first dirtied the page
   since its last flush - both "is this frame dirty" and the Dirty Page
   Table's `recLSN` in one field, rather than the separate `AtomicBool`
   a naive reading of "one atomic per fact" would suggest, since
   `dirty_page_table` needs the actual LSN, not just a flag). Frames live
   in a `Box<[Frame]>` allocated once in `BufferPool::new` and never
   resized, so a `&'pool Frame` reference (through `BufferPool`, never
   held bare) stays valid for the pool's whole lifetime.

**The latch ordering rule - read this before touching `fetch_page`,
`new_page`, `acquire_free_frame`, or `flush_pages`:**

1. Acquire `index`'s mutex.
2. Find or reserve a frame (page-table hit: pin it in place; miss:
   pop the free list or ask the replacer to evict, removing the
   victim's old mapping).
3. Release the mutex.
4. *Then*, if the frame's content needs to change (a fresh disk read, a
   brand-new page's blank content, or an eviction victim's dirty bytes
   being flushed), touch the frame's own `RwLock` or perform I/O.
5. Re-acquire the mutex only for the final, short step of recording a
   newly-installed page's `PageId -> FrameId` mapping.

Never hold `index`'s mutex while taking a frame's `RwLock` or performing
disk I/O - `acquire_free_frame` selects and reserves a victim frame under
the mutex, then releases it *before* flushing that victim if it was
dirty (`flush_pages` takes each frame's `RwLock` for a read-only
snapshot, never the pool mutex). This is the rule M10.2's lock manager
will be tempted to violate by taking a shortcut that holds the pool
mutex "just for this one flush" - don't.

`PageReadGuard`/`PageWriteGuard` (`page.rs`) hold the frame's `RwLock`
guard (wrapped in `ManuallyDrop`, so `Drop` can release it explicitly)
plus the pin; `Drop` releases the latch first (`ManuallyDrop::drop`),
*then* decrements the pin (`unpin_frame`) - releasing the pin while
still holding the latch would let the frame be evicted and reinstalled
out from under a latch that hasn't actually been released yet. Today
every ordinary caller above `storage` still goes through `fetch_page`
(returning a `PageWriteGuard`, since call sites like `btree::Node`
interleave reads and writes on the same guard) or `new_page` (always a
fresh, blank `PageWriteGuard`); `fetch_page_read` (a genuine `RwLock`
read latch, returning `PageReadGuard`) exists for callers that only ever
read - `BufferPool` uses it itself for its own read-only header/LSN
accessors, and `crates/storage/tests/buffer_pool_concurrency.rs` uses it
to prove concurrent readers actually work.

## Key Components

- `Frame` - private. See the field-by-field breakdown above.
- `PoolIndex` - private. See above.
- `BufferPool` - `disk_manager`, `dwb`, and `log_manager` need no
  external lock of their own (each is internally synchronized - see
  `disk.MD`, `dwb.MD`, `wal.MD`); `frames` and `index` are described
  above.
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
  test/`test-util`-only introspection: the number of `fetch_page`/
  `fetch_page_read` calls made so far (for tests asserting an operation
  touched only the pages it needed), and every disk write `flush_pages`
  has performed, in order.
- `BufferPool::fetch_page(&self, page_id) -> Result<PageWriteGuard<'_>, StorageError>` -
  fetches `page_id`, pinning it and returning a write-capable guard. If
  the page is not already resident, brings it in from disk, evicting a
  victim frame via the replacer if none is free. Emits a `trace!` naming
  `page_id` on every call (shared with `fetch_page_read` via
  `fetch_frame`) - this is the hottest path in the crate, so the event is
  built from the already-owned `page_id.0` with no `format!`/allocation
  outside the macro, cheap enough to leave enabled even though it's
  almost never turned on. Also increments `buffer_pool_hits_total` or
  `buffer_pool_misses_total` (`metrics` crate counters - see
  `docs/ROADMAP.md`'s M13 entry) depending on which branch is taken - the
  hit ratio these two produce is, per that entry, "the single most
  diagnostic number in a database."
- `BufferPool::fetch_page_read(&self, page_id) -> Result<PageReadGuard<'_>, StorageError>` -
  the read-only counterpart: same fetch/install/evict logic
  (`fetch_frame`), but returns a guard backed by the frame's `RwLock`
  read latch instead of its write latch, so multiple readers of the same
  frame can hold a guard at once.
- `BufferPool::fetch_frame(&self, page_id) -> Result<FrameId, StorageError>` -
  private. The fetch/pin/evict/install logic shared by `fetch_page` and
  `fetch_page_read`; the two public methods differ only in which kind of
  guard they wrap the resulting `FrameId` in.
- `BufferPool::new_page(&self, txn_id) -> Result<(PageId, PageWriteGuard<'_>), StorageError>` -
  allocates a brand-new page via the disk manager and pins it in the
  pool, returning a write guard to it. Logs an `AllocPage` record on
  behalf of `txn_id` before installing the page.
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
  behalf of `txn_id` - an ordinary, logged `PageWriteGuard::write` like
  any other page mutation, so it is redone or undone exactly like the
  catalog heap allocation it accompanies rather than racing ahead of it
  as an unlogged direct write.
- `BufferPool::index_catalog_first_page(&self) -> Result<Option<PageId>, StorageError>`/
  `set_index_catalog_first_page(&self, txn_id, page_id)` - the index
  catalog's first page, read and written identically to
  `catalog_first_page`/`set_catalog_first_page` above but at
  `disk::header::INDEX_CATALOG_FIRST_PAGE_RANGE` instead - a second,
  independent header slot rather than a field inside the table catalog's
  own rows, since `catalog::Catalog` bootstraps and grows the two heaps
  (tables, indexes) on separate schedules (`catalog::catalog.MD`).
- `BufferPool::append_log(&self, txn_id, kind) -> Result<Lsn, StorageError>` -
  appends `kind` to the write-ahead log on behalf of `txn_id`, returning
  its assigned LSN. Does not by itself make the record durable; call
  `flush_log`/`flush_log_all` for that.
- `BufferPool::flush_log(&self, up_to)`/`flush_log_all(&self)` - forces
  log records up to `up_to`, or every record appended so far, to disk.
- `BufferPool::durable_lsn(&self) -> Lsn` - the write-ahead log's current
  durable LSN.
- `BufferPool::pin_if_cached(&self, page_id) -> Option<FrameId>` -
  private. The page-table-hit half of `fetch_frame`: under `index`'s
  mutex, looks up `page_id` and pins its frame in place if found.
- `BufferPool::pin_locked(&self, index, frame_id)` - private. Assumes
  `index`'s mutex is already held by the caller (`pin_if_cached` or
  `install`); increments the frame's pin count and tells the replacer
  it's no longer evictable, moving the `buffer_pool_pinned_frames` gauge
  up.
- `BufferPool::acquire_free_frame(&self) -> Result<FrameId, StorageError>` -
  private. Finds a frame to hold a newly-fetched or newly-allocated page:
  reuses a never-used frame if one is free, otherwise asks the replacer
  for an evictable victim under `index`'s mutex, removes the victim's old
  mapping, *releases the mutex*, and only then flushes the victim if it
  was dirty (the latch-ordering rule above). Emits a `trace!` naming the
  evicted page when a victim is chosen (incrementing
  `buffer_pool_evictions_total` alongside it), and a `warn!` (every frame
  pinned - eviction pressure an operator should know about) right before
  returning `StorageError::BufferPoolExhausted`.
- `BufferPool::install(&self, frame_id, page_id, page, dirty_since_lsn)` -
  private. Stores `page` into `frame_id`'s `RwLock` (a plain write latch;
  nothing else can reach this frame by `page_id` yet), then records it
  under `page_id` in the page table and pins it, both under `index`'s
  mutex. Only ever called on a frame that was just freed or evicted, so
  no live guard can be observing its bytes. `dirty_since_lsn` is
  `Some(lsn)` if the page is already dirty as of installation (a
  brand-new page, dirtied by its own `AllocPage` record), or `None` for a
  page freshly read from disk.
- `BufferPool::write_guard(&self, page_id, frame_id) -> PageWriteGuard<'_>`/
  `read_guard(&self, page_id, frame_id) -> PageReadGuard<'_>` - private.
  Take the frame's `RwLock` write/read latch and wrap it in the
  corresponding guard type. Called only after the frame is already
  pinned (by `fetch_frame` or `new_page`).
- `BufferPool::unpin_frame(&self, frame_id)` - private. Decrements a
  frame's pin count (an `AtomicU32`, so this itself needs no lock); if
  that was the last pin, acquires `index`'s mutex just long enough to
  tell the replacer the frame is evictable again. Moves the
  `buffer_pool_pinned_frames` gauge down.
- `BufferPool::flush_pages(&self, pages) -> Result<(), StorageError>` -
  private. Emits a `trace!` naming the batch size before doing anything,
  and increments `dwb_batches_written_total` once the batch's double-write
  copy is durable (a nonzero rate here, per `docs/ROADMAP.md`'s M13 entry,
  is expected steady-state traffic, unlike `dwb_pages_restored_total` -
  see `recovery.MD` - which should be zero outside a crash). Flushes a
  batch of currently-dirty, resident pages to disk through the
  double-write buffer, latching each frame only long enough to clone a
  consistent snapshot of its bytes (never holding `index`'s mutex during
  any of this). Every entry in `pages` must already be dirty and safe
  from concurrent reassignment; this does not re-check either condition.
  Implements the write-ahead rule (the log must be durable to the
  batch's *highest* `page_lsn` before anything reaches the double-write
  buffer - not before each page individually, and not merely before the
  real file, which is what made a torn write unrecoverable before the
  double-write buffer existed) and the double-write protocol itself:
  write every page's image to `dwb` and sync it (so the batch is now
  recoverable even if nothing below has happened yet), only then write
  every page to its real location and sync that, then clear `dwb`. A
  crash at any point along the way leaves either a real page untouched by
  this batch, or a torn real page with an intact `dwb` copy
  `recovery::recover_double_write` can restore from - never a torn real
  page with no way back.
- `BufferPool::frame_of(&self, page_id) -> Result<FrameId, StorageError>` -
  private.
- `BufferPool::dirty_page_table(&self) -> Vec<(PageId, Lsn)>` - the Dirty
  Page Table: every currently-dirty resident page and its
  `recovery_lsn` (the LSN of the record that first dirtied it since its
  last flush, read straight off each frame's `dirty_since_lsn` atomic).
  A checkpoint snapshot of this is what lets recovery's Redo pass start
  from the oldest change that might still be missing from disk, rather
  than the beginning of the log.
- `BufferPool::last_checkpoint_lsn(&self)`/`set_last_checkpoint_lsn(&self, txn_id, lsn)` -
  read/write the most recent checkpoint's `CheckpointBegin` LSN in the
  database header, through the same logged `PageWriteGuard::write` path
  as `catalog_first_page`.
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
  resident (via `fetch_page_read` - this never mutates). Recovery-only:
  lets the Redo pass decide whether a record needs replaying.
- `BufferPool::stamp_write(&self, page_id, offset, bytes, lsn)` -
  recovery-only: applies `bytes` at `offset` on `page_id` and stamps its
  `page_lsn` to `lsn`, marking the frame dirty, *without* appending a new
  WAL record. Bypasses the ordinary log-then-mutate discipline
  `PageWriteGuard::write` enforces (by mutating straight through the
  write guard's held latch instead of calling `write`), which is correct
  here only because the caller is either replaying a record that is
  already durably logged (Redo) or has just logged a `Clr` covering
  exactly this write itself (Undo) - in both cases logging again would
  give the log a false description of what happened.
- `PageReadGuard::page(&self) -> &Page` - read-only access to the pinned
  page.
- `impl Drop for PageReadGuard` - releases the read latch, then unpins.
- `PageWriteGuard::page(&self) -> &Page` - read-only access to the pinned
  page (a write guard can always read; `btree::Node`/`heap::SlottedPage`
  read the current bytes before computing a patch to `write`).
- `PageWriteGuard::write(&mut self, txn_id, offset, bytes)` - the only
  way to mutate a pinned page's bytes through the ordinary path: captures
  the current bytes at `offset..offset+bytes.len()` as the before-image,
  appends an `Update` record on behalf of `txn_id` (before-image for
  undo, `bytes` for redo), applies `bytes` to the page, and stamps the
  page's `page_lsn` with the record's assigned LSN. Marks the frame
  dirty (setting `dirty_since_lsn` only if it was previously clean, so
  the recorded `recLSN` is always the *first* dirtying write, per ARIES).
- `impl Drop for PageWriteGuard` - releases the write latch, then unpins.
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

`heap::SlottedPage`/`TableHeap` and `btree::Node`/`BTreeIndex` are
`BufferPool`'s main callers above `storage`: every tuple insert, delete,
and scan goes through `fetch_page`/`fetch_page_read`/`new_page` to get a
guard, and `PageWriteGuard::write` to mutate it - read-only descents and
scans use `fetch_page_read`, anything that may split or rebuild a node
uses `fetch_page`. `recovery::recover` drives the pool directly during
Redo/Undo via `stamp_write`/`page_lsn`/`log_iter_from`/`read_log_at`.
`engine::Database` calls `flush_all`/`sync`/`flush_log_all` at clean
shutdown.
