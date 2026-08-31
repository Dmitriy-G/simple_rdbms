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
   its reverse mapping (`frame_page`), the free list, the replacer,
   `owned` (a `Vec<bool>` marking which frames are currently reserved by
   an in-flight `acquire_free_frame`/`try_install` pair, per
   `docs/adr/0009-buffer-pool-frame-ownership.md`), `loading` (a
   `HashSet<PageId>` of pages a miss is currently reading from disk for,
   per `docs/adr/0010-buffer-pool-waits-for-a-frame.md`), and `evicting` (a
   `HashSet<PageId>` of pages currently being written out by an in-flight
   eviction, the eviction-visibility rule below), held together in one
   struct since every mutation to one of them (installing a page,
   evicting a victim) always needs to update the others atomically with
   it. Held only for lookups and frame bookkeeping - **never** across a
   frame latch acquisition or disk I/O; the two `Condvar`s below (paired
   with this same mutex) are the one exception that's expected to block
   while holding it, since `Condvar::wait`/`wait_timeout` release the
   mutex for the duration of the wait by construction.
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
the mutex, marking its page id `evicting` rather than removing it from
`page_table` (the eviction-visibility rule below), then releases the
mutex *before* flushing that victim if it was dirty (`flush_pages` takes
each frame's `RwLock` for a read-only snapshot, never the pool mutex).
This is the rule M10.2's lock manager
will be tempted to violate by taking a shortcut that holds the pool
mutex "just for this one flush" - don't. `fetch_frame` returns a bare
`FrameId` and never takes a frame latch itself - `fetch_page`/
`fetch_page_read` take it afterward, in `write_guard`/`read_guard` - so
the shared fetch/pin/evict/install logic already follows this structurally;
M10.2 will be tempted to inline pinning and latching into one step, and
doing so would reintroduce exactly the ordering this rule forbids.

**The flush-sequence rule - read this before touching `flush_pages`:**
`flush_sequence` (a `Mutex<()>`) is acquired *after* `index`'s mutex has
already been released and *before* any frame latch the flush needs -
including the read latch each page's snapshot is cloned under - and is
held across the whole flush: the per-page snapshot, `log_manager.flush`
up to the batch's highest `page_lsn`, `dwb.write_batch`, every real page
write, `disk_manager.sync`, `dwb.clear_batch`, and the loop that clears
each frame's `dirty_since_lsn` once its snapshot's `page_lsn` is confirmed
still current - not just one call in the middle of it. This width matters:
narrower coverage (once acquired only around `write_batch`/`clear_batch`)
still let two concurrent flushes of the same frame interleave their
snapshot-then-write steps and land out of order - an older snapshot's
bytes written last, over a newer snapshot's, with the frame already marked
clean by the newer flush's dirty-clear - since nothing serialized "read
this frame's current bytes" against another flush doing the same read and
write around it. None of `flush_pages`'s three callers (`flush_page`,
`flush_all`, `acquire_free_frame`) holds a page latch when they call in,
so lock order is `flush_sequence` -> frame latch, never the reverse - a
future caller that already holds a frame latch before calling in would
invert the order every other path in this file assumes. See the poisoning
paragraph below for what the lock protects and why per-call atomicity of
`write_batch`/`clear_batch` alone was not enough.

`try_install` (below) takes one narrow, deliberate exception: it writes a
freshly-read page's bytes into its frame's `RwLock` *while still holding*
`index`'s mutex, rather than after releasing it. This is safe only because
the frame is not yet reachable by any other thread - it is in neither
`page_table` nor `frame_page` until the `insert` a few lines later, so no
concurrent `pin_if_cached` can find it and no `acquire_free_frame` can
evict it. Releasing the mutex first and writing the bytes after would open
a window where the mapping is visible but the frame still holds its
previous occupant's bytes (or, for a brand-new frame, zeroes) - a
concurrent reader could then latch and read stale content before the
installing thread gets around to overwriting it. Holding the mutex the
extra instant it takes to copy one page's worth of bytes into an
as-yet-unpublished frame costs nothing, since nothing else can be waiting
on that specific frame yet.

**The ownership rule - read this before touching `acquire_free_frame`,
`try_install`, or `unpin_frame`:** the frame `acquire_free_frame` returns
is unpinned, out of the free list, and (on the eviction path) already
removed from the replacer - nothing else marks it as spoken for while
the caller does the disk read or `append_log` that comes next with no
lock held. `PoolIndex::owned[idx]` closes that gap:
`acquire_free_frame` sets it before releasing the mutex, `try_install`
clears it (on both the winning and the losing branch) before returning,
and `unpin_frame`'s pin-count re-check additionally requires
`!owned[idx]` before telling the replacer a frame is evictable again.
That last check is the actual fix for a real bug
(`docs/adr/0009-buffer-pool-frame-ownership.md`): `unpin_frame`
decrements a frame's pin count outside the mutex, and its later re-check
under the mutex is stale by construction - if the frame was evicted in
the gap, the re-check still reads pin count zero and would otherwise
resurrect a frame `try_install` has not published yet, letting a second,
unrelated eviction steal it out from under its first owner. Both
`acquire_free_frame` and `try_install` carry a `debug_assert!` on
`owned[idx]`'s expected value at entry, documenting the handoff and
catching any future path that bypasses it. `LruKReplacer::set_evictable`
cooperates: it only ever mutates an entry `record_access` already
created, never inserts one, since a frame absent from the replacer is by
definition either owned or free, and marking it evictable in either case
would be wrong (`replacer.MD`).

**The waiting rule - read this before touching `acquire_free_frame` or
`fetch_frame`'s miss path:** `acquire_free_frame` no longer fails the
instant nothing is free; it loops, waiting on `frame_available` (a
`Condvar` paired with `index`'s mutex, bounded by `frame_wait_timeout`,
default `DEFAULT_FRAME_WAIT_TIMEOUT` = 30s) until `unpin_frame` or
`try_install`'s losing branch notifies it that a frame became available,
retrying the free-list/eviction check on every wake. A timeout returns
`StorageError::BufferPoolWaitTimedOut`; the one case waiting can never fix
- every frame already pinned by the *calling* thread - is detected
immediately instead, via a `#[cfg(debug_assertions)]` thread-local pin
count (`PINNED_FRAME_COUNTS`, incremented by `pin_locked` and decremented
by `unpin_frame`, mirroring `HELD_WRITE_GUARD_FRAMES` below), and returns
`StorageError::BufferPoolExhausted` without waiting
(`docs/adr/0010-buffer-pool-waits-for-a-frame.md`).

Real waiting turned a second, previously-safe-by-narrow-timing property
into a bug: `fetch_frame`'s miss path used to let every thread racing a
miss on the same page independently read it from disk, trusting
`try_install`'s double-checked page-table insert to discard every loser's
copy. That trust assumed the gap between a miss and its `try_install`
call stayed a few instructions wide; once `acquire_free_frame` can wait
for real time, a loser's stale disk read can outlive an entire
write-then-evict cycle for the winner's copy and overwrite it. `loading`
closes this: a missing thread claims `page_id` in `loading` before doing
any I/O; a thread that finds it already claimed waits on `page_installed`
(the second `Condvar` on `index`'s mutex) instead of reading disk itself,
rechecking the page table on every wake. Only the claimant reads disk and
calls `try_install`; the claim is released and every waiter woken
(`notify_all`, since more than one page id's misses can be waiting)
on every exit from the load, success or failure alike - see
`fetch_frame`'s closure below for why that matters.

**The eviction-visibility rule - read this before touching
`acquire_free_frame`, `pin_if_cached`, or `fetch_frame`'s hit path:**
a victim page's `page_table`/`frame_page` entries stay in place for the
*entire* duration of its eviction flush, not just until the mutex is
released. `acquire_free_frame` records the victim's page id in
`evicting` instead of removing it; `pin_if_cached` and the mutex-held
loop in `fetch_frame` both check `evicting` before trusting a
`page_table` hit, falling through to wait on `page_installed` (the same
condvar and pattern `loading` uses) if the page they want is mid-flush,
rather than pinning a frame about to be repurposed for someone else's
page. This closes a real bug: the previous code removed the victim's
mapping *before* flushing it, so a concurrent fetch for that same page
saw a miss, read the stale on-disk copy (the flush hadn't reached disk
yet), and installed a second, older copy of the page into a different
frame - whichever frame's writer ran last would silently win, and the
loser's writes vanished. `acquire_free_frame` resolves `evicting` one of
two ways once the flush attempt finishes: `complete_eviction` (the flush
succeeded, or the victim was never dirty) removes the now-stale
`page_table`/`frame_page` entries and clears `evicting`, so the next
waiter's re-check misses and does a fresh disk read - one that now finds
the just-flushed content; `abort_eviction` (the flush failed) instead
restores the frame to an ordinary, evictable, `owned = false` resident
frame with its mapping untouched - as if the eviction attempt never
started - and re-`record_access`es it with the replacer, whose `evict`
removed its entry entirely rather than merely marking it unevictable.
Both paths call `page_installed.notify_all()` so every waiter re-checks;
`abort_eviction` additionally calls `frame_available.notify_one()`,
since a frame just became evictable again for any thread still waiting
on a free frame.

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

**The one-write-guard-per-page rule - read this before calling `fetch_page`
a second time without dropping the first:**

- A thread may hold at most one write guard on a given page at a time.
  Asking for a second - a second `fetch_page`/`new_page` on the same page
  before the first guard drops - is a self-deadlock: `frames[idx].page`
  is a plain `std::sync::RwLock`, which is not reentrant, so the second
  call blocks forever on a latch this same thread already holds. Debug
  builds detect this (see `try_install`'s neighbor `write_guard` and its
  thread-local `HELD_WRITE_GUARD_FRAMES` set below) and panic naming the
  page instead of letting the thread park.
- Multiple read guards on one page are always fine, whether they belong
  to one thread or many - `RwLock::read` is reentrant-safe and shared by
  design. A read guard also never blocks behind another read guard, only
  behind a live write guard.
- `new_page` and `fetch_page` return write guards; reach for
  `fetch_page_read` whenever a call site only ever reads the page.
  Prefer the read form where either would do - it is the one that
  composes: two read guards on the same page from the same thread (or
  different ones) coexist without incident, where two write guards
  self-deadlock and a write-then-read on the same page from one thread
  only works today because it happens to reuse the *same* guard rather
  than acquiring a second one.

This is a real behavioral change from before per-frame `RwLock`s existed
(`docs/adr/0008-write-guard-reentrancy.md`): when `PageGuard`
was purely a pin with no latch behind it, a thread holding two guards to
the same page was ordinary and several of this crate's own tests relied
on it. `BufferPool::write_guard` is the only place a write guard is
minted, so it is the one place this rule can be enforced centrally: under
`#[cfg(debug_assertions)]`, a thread-local `HashSet<FrameId>`
(`HELD_WRITE_GUARD_FRAMES`) records every frame this thread currently
holds a write guard on; `write_guard` panics if the frame is already in
the set, and `PageWriteGuard::drop` removes it. Compiled out entirely
under `#[cfg(not(debug_assertions))]` - a release build pays nothing for
this and, symmetrically, gets no protection from it; the panic exists to
catch the mistake in development and CI, not to guard production against
one it should never reach in the first place. `PageReadGuard` needs no
equivalent set, since read guards never conflict with each other on the
same thread.

## Key Components

- `Frame` - private. See the field-by-field breakdown above.
- `PoolIndex` - private. See above.
- `InstallHook` - `#[cfg(feature = "test-util")]`, private
  type alias for `dyn Fn(FrameId) + Send + Sync`, the shape of the
  closure `set_install_hook` accepts.
- `BufferPool` - `disk_manager`, `dwb`, and `log_manager` need no
  external lock of their own (each is internally synchronized - see
  `disk.MD`, `dwb.MD`, `wal.MD`); `frames` and `index` are described
  above. `frame_available`/`page_installed` are the two `Condvar`s the
  waiting rule above describes, both paired with `index`'s mutex.
  `frame_wait_timeout` bounds every wait either one performs, set from
  `DEFAULT_FRAME_WAIT_TIMEOUT` in `new` and overridable via
  `with_frame_wait_timeout`. `install_hook` (test/`test-util`-only) holds
  the optional `InstallHook` closure `call_install_hook` invokes.
  `flush_poisoned: AtomicBool` starts `false` and is set permanently once
  `flush_pages` fails after handing a batch's images to `dwb` but before
  retiring them - see `flush_pages` below for why that specific window is
  the one that matters. `flush_sequence: Mutex<()>` serializes the
  double-write sequence itself (the flush-sequence rule above) so that at
  most one batch is ever outstanding in `dwb` at a time.
- `WriteObservation` - test/`test-util`-only. One observed disk write, for
  the WAL-ordering invariant test: at the moment `page_id` was written to
  disk, its `page_lsn` and the log's `durable_lsn` at that instant. The
  write-ahead rule requires `durable_lsn >= page_lsn` for every entry.
- `BufferPool::DEFAULT_FRAME_WAIT_TIMEOUT` - `Duration::from_secs(30)`,
  the default bound `acquire_free_frame` and `fetch_frame`'s `loading`
  wait use before giving up with `StorageError::BufferPoolWaitTimedOut`.
- `BufferPool::new(disk_manager, dwb, log_manager, pool_size, replacer)` -
  creates a buffer pool of `pool_size` frames, using `replacer` to choose
  eviction victims, logging every page mutation through `log_manager`
  before it is applied, and protecting every flush against a torn write
  through `dwb` (see `flush_pages`). Starts with `frame_wait_timeout` set
  to `DEFAULT_FRAME_WAIT_TIMEOUT`.
- `BufferPool::with_frame_wait_timeout(self, timeout) -> Self` - builder
  method overriding `frame_wait_timeout`; takes `self` by value rather
  than `&self` since every existing call site constructs a pool and
  wraps it in `Arc` in the same expression (`Arc::new(BufferPool::new(...)
  .with_frame_wait_timeout(t))`), so a consuming builder needs no new
  interior mutability and costs the ~25 existing `BufferPool::new` call
  sites nothing to leave unchanged.
- `BufferPool::fetch_count`/`reset_fetch_count`/`write_observations` -
  test/`test-util`-only introspection: the number of `fetch_page`/
  `fetch_page_read` calls made so far (for tests asserting an operation
  touched only the pages it needed), and every disk write `flush_pages`
  has performed, in order.
- `BufferPool::frame_wait_count(&self) -> usize` - test/`test-util`-only
  introspection: how many times `acquire_free_frame` has actually waited
  on `frame_available` so far, mirroring the `buffer_pool_frame_waits_total`
  metric counter without needing a metrics recorder installed in a test
  process. A concurrency test driving a pool smaller than its thread
  count should see this go non-zero - if it doesn't, the test isn't
  exercising the waiting path it's meant to.
- `BufferPool::frame_count_for(&self, page_id) -> usize` -
  test/`test-util`-only introspection: how many frames currently list
  `page_id` in `frame_page`. Always `0` or `1` once `try_install`'s
  double-checked insert is correct; a concurrency test that drives two
  threads to miss on the same page at once and then observes `2` here has
  reproduced the M10.1 double-fetch race this module's `try_install` was
  written to close.
- `BufferPool::set_install_hook(&self, hook)`/`clear_install_hook(&self)` -
  test/`test-util`-only. Installs (or removes) a closure
  `call_install_hook` invokes with the just-acquired `FrameId` between
  `acquire_free_frame` and `try_install`, in both `fetch_frame` and
  `new_page`. Lets a test pause a specific thread in that window on
  demand - the exact gap `docs/adr/0009-buffer-pool-frame-ownership.md`'s
  bug lived in - instead of relying on the OS scheduler to land there by
  chance.
- `BufferPool::call_install_hook(&self, frame_id)` - private,
  test/`test-util`-only. Locks `install_hook` and invokes it if set;
  a no-op with no lock contention on the hot path when no hook is
  installed (the common case even under `test-util`, since only tests
  that specifically call `set_install_hook` pay for it).
- `BufferPool::assert_frame_accounting(&self)` - test/`test-util`-only.
  Walks `page_table` and asserts, for every entry, that `frame_page` at
  that frame agrees on the page id and that no frame id is visited twice;
  then asserts every frame in `free_list` has no `frame_page` entry and
  is not `owned`; then asserts every `owned` frame is absent from both
  `page_table` and `free_list` - the full set of cross-invariants
  `PoolIndex`'s four collections (`page_table`, `frame_page`, `free_list`,
  `owned`) must maintain together, and the ways
  `docs/adr/0009-buffer-pool-frame-ownership.md`'s and
  `docs/adr/0010-buffer-pool-waits-for-a-frame.md`'s bugs (and the M10.1
  double-fetch race before them) showed up as corrupted pool state.
  Intended to run at the end of any test that drives the pool
  concurrently, alongside whatever content-level assertions that test
  already makes.
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
  guard they wrap the resulting `FrameId` in. On a miss, claims `page_id`
  in `loading` (waiting on `page_installed` first if another thread
  already claimed it - the waiting rule above), then runs the actual
  load - `call_install_hook`, `acquire_free_frame`, the disk read, and
  `try_install` - inside a closure so every exit path, including
  `acquire_free_frame`'s own `?`, still reaches the code after it that
  clears `loading` and calls `notify_all`; an early return straight out
  of `fetch_frame` would leave `page_id` claimed forever, hanging every
  future fetch of it. A disk-read failure additionally calls
  `release_owned_frame` before propagating the error, since
  `acquire_free_frame` already marked the frame `owned` and nothing else
  will release it.
- `BufferPool::release_owned_frame(&self, frame_id)` - private. Clears
  `owned[frame_id]`, pushes it onto the free list, and notifies
  `frame_available` - the cleanup a caller that acquired a frame via
  `acquire_free_frame` but then failed before reaching `try_install` must
  run, or the frame leaks as permanently `owned` (a slow pool death, not
  corruption, since `try_install`/`acquire_free_frame`'s `debug_assert!`s
  only catch a frame reused while still `owned`, not one abandoned and
  never reused at all).
- `BufferPool::new_page(&self, txn_id) -> Result<(PageId, PageWriteGuard<'_>), StorageError>` -
  allocates a brand-new page via the disk manager and pins it in the
  pool, returning a write guard to it. Logs an `AllocPage` record on
  behalf of `txn_id` before installing the page. Routes through
  `try_install` like `fetch_frame` does, even though it can never actually
  race: `page_id` comes from `disk_manager.allocate_page`'s atomic
  counter, so no two callers can ever be handed the same id to install,
  and `try_install`'s re-check is guaranteed to find the page table empty.
  Sharing the one helper avoids maintaining a second, subtly-different
  install path whose race-freedom would otherwise depend on this same
  atomic-allocation fact without saying so anywhere. Also calls
  `call_install_hook` right after `acquire_free_frame`, for the same
  reason `fetch_frame` does; nothing between that call and `try_install`
  can return early today, so the acquired frame's `owned` flag is always
  cleared, but the ownership rule's `debug_assert!`s catch it immediately
  if a future edit changes that.
- `BufferPool::flush_page(&self, page_id)` - flushes `page_id` to disk if
  it is resident and its frame is dirty; a non-resident `page_id` is a
  silent `Ok(())`, not `StorageError::PageNotFound`, because a page that
  isn't in the pool at all is already durable - eviction only ever
  removes a page from `page_table` after successfully flushing it if it
  was dirty (the eviction-visibility rule above), so absence itself is
  proof there is nothing left to flush. This matters beyond an odd corner
  case: `txn::write_checkpoint`'s `pool.flush_page(PageId(0))` call is a
  real, live dependency on this - page 0 (the database header) is by far
  the coldest page in the pool, and under enough eviction pressure from
  everything else a checkpoint touches, it can easily have been evicted
  (and its own dirty bytes already durable) by the time the checkpoint
  gets to this call. Before this returned `Ok(())` for the non-resident
  case, that ordinary eviction would have surfaced as a checkpoint
  failure with no real error underneath it.
- `BufferPool::flush_all(&self)` - flushes every dirty resident page to
  disk, batching them through the double-write buffer in groups of at
  most `dwb.capacity()` pages - one double-write round trip per group
  rather than per page, which is where batching actually pays for the
  doubled writes `flush_pages` costs. `DoubleWriteBuffer::open` rejects
  `capacity == 0`, so the chunk size here is always positive.
- `BufferPool::sync(&self)` - forces every write made so far to durable
  storage. Callers should `flush_all` first; this only syncs whatever has
  already reached the disk manager.
- `BufferPool::is_flush_poisoned(&self) -> bool` - whether `flush_poisoned`
  is currently set. Lets a caller above `storage` - `engine::Database`,
  see `database.MD` - notice the fatal condition and refuse further work
  immediately instead of discovering it only once eviction can no longer
  find a clean victim to reclaim.
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
  mutex, returns `None` immediately if `page_id` is in `evicting` (the
  eviction-visibility rule above), otherwise looks up `page_id` and pins
  its frame in place if found.
- `BufferPool::pin_locked(&self, index, frame_id)` - private. Assumes
  `index`'s mutex is already held by the caller (`pin_if_cached` or
  `install`); increments the frame's pin count and tells the replacer
  it's no longer evictable, moving the `buffer_pool_pinned_frames` gauge
  up.
- `BufferPool::acquire_free_frame(&self) -> Result<FrameId, StorageError>` -
  private. Finds a frame to hold a newly-fetched or newly-allocated page:
  loops trying the free list, then the replacer for an evictable victim,
  under `index`'s mutex; if neither has anything, checks (debug builds
  only) whether the calling thread itself already holds every frame
  pinned - a guaranteed self-deadlock, returning
  `StorageError::BufferPoolExhausted` immediately - and otherwise waits
  on `frame_available` up to the remaining `frame_wait_timeout` before
  retrying the loop, or returns `StorageError::BufferPoolWaitTimedOut`
  once the deadline passes (the waiting rule above). Once a frame is
  found, marks the victim's page id `evicting` *without* touching
  `page_table`/`frame_page` yet (the eviction-visibility rule above), and
  marks the chosen frame `owned` (the ownership rule above, behind a
  `debug_assert!` that it was not already owned); *releases the mutex*,
  and only then flushes the victim if it was dirty (the latch-ordering
  rule above). A flush failure calls `abort_eviction` and propagates the
  error rather than leaving the frame `owned` forever with no mapping
  restored - the fix for a real frame leak, since every failed eviction
  flush used to cost the pool a frame permanently. A flush success (or a
  victim that was never dirty) calls `complete_eviction` to finally
  retire the victim's mapping. Emits a `trace!` naming the evicted page
  when a victim is chosen (incrementing `buffer_pool_evictions_total`
  alongside it), and increments `buffer_pool_frame_waits_total`/
  `frame_wait_count` and records `buffer_pool_frame_wait_seconds` around
  each wait.
- `BufferPool::abort_eviction(&self, frame_id, victim_page_id)`/
  `complete_eviction(&self, frame_id, victim_page_id)` - private. The two
  ways `acquire_free_frame` resolves an in-flight eviction (the
  eviction-visibility rule above); both clear `evicting` and
  `notify_all` on `page_installed` so any thread waiting on
  `victim_page_id` re-checks. `abort_eviction` additionally clears
  `owned`, re-inserts the frame into the replacer as evictable (`evict`
  removed its entry outright, unlike `set_evictable(false)`, so a plain
  `set_evictable(true)` alone would be a no-op), and calls
  `frame_available.notify_one()`, leaving the frame exactly as it was
  before the eviction attempt - still mapped, still resident, just
  available again. `complete_eviction` instead removes the victim's
  `page_table`/`frame_page` entries now that its content is safely on
  disk (or was never dirty), finally freeing the frame to be repurposed
  by the caller's own subsequent `try_install`.
- `BufferPool::try_install(&self, frame_id, page_id, page, dirty_since_lsn) -> FrameId` -
  private. The double-checked publish step both `fetch_frame` and
  `new_page` funnel through: under `index`'s mutex (behind a
  `debug_assert!` that `frame_id` is still marked `owned` - the ownership
  rule above), re-checks whether `page_id` is already present in the page
  table before doing anything else. If it is - another thread's
  `try_install` for the same page won the race first - `frame_id`'s
  `owned` flag is cleared, `frame_id` (the caller's freshly-read-into but
  never-published frame) is pushed back onto the free list unpinned, the
  winner's frame is pinned in its place, `frame_available.notify_one()`
  runs (a frame just returned to the free list), and the winner's
  `FrameId` is returned; the caller's own read was wasted work but leaves
  nothing to clean up, since `frame_id` was never recorded in `page_table`
  or `frame_page` and its bytes were never touched. `fetch_frame`'s
  `loading` claim (the waiting rule above) means this branch is no longer
  reachable from an ordinary miss - only one thread ever loads a given
  page at a time now - but it stays as the correct, defensive behavior for
  any caller that reaches `try_install` outside that protection.
  Otherwise this thread has
  won: it stores `page` into `frame_id`'s `RwLock`, sets
  `dirty_since_lsn` (`Some(lsn)` for a brand-new page dirtied by its own
  `AllocPage` record, `None` for a page freshly read from disk), records
  the mapping in `page_table`/`frame_page`, clears `frame_id`'s `owned`
  flag, pins it, and returns `frame_id` unchanged. The bytes write
  happens only in the winning branch, and only after the re-check
  confirms there is a race to win - writing first and checking after (the
  pre-M10.2-fix shape of this function) is what let a losing thread
  clobber a frame it was about to hand back. See the latch-ordering rule
  above for why this is the one place in the module that deliberately
  holds `index`'s mutex across a frame latch acquisition. Only ever
  called with a frame that was just freed or evicted, so no live guard
  can already be observing its bytes going in - and, since
  `docs/adr/0009-buffer-pool-frame-ownership.md`, no other thread's
  `acquire_free_frame` can have evicted it out from under this call
  either, since `owned` stays set until this function clears it.
- `BufferPool::write_guard(&self, page_id, frame_id) -> PageWriteGuard<'_>`/
  `read_guard(&self, page_id, frame_id) -> PageReadGuard<'_>` - private.
  Take the frame's `RwLock` write/read latch and wrap it in the
  corresponding guard type. Called only after the frame is already
  pinned (by `fetch_frame` or `new_page`). In debug builds, `write_guard`
  checks and inserts `frame_id` into `HELD_WRITE_GUARD_FRAMES` (below)
  *before* taking the actual `RwLock` write latch, panicking instead of
  blocking if this thread already holds a write guard on this frame -
  see the one-write-guard-per-page rule above.
- `HELD_WRITE_GUARD_FRAMES` - `#[cfg(debug_assertions)]`, private,
  thread-local. The `HashSet<FrameId>` `write_guard`/
  `PageWriteGuard::drop` insert into and remove from to detect a
  same-thread double write guard. Absent entirely in release builds.
- `BufferPool::unpin_frame(&self, frame_id)` - private. Decrements a
  frame's pin count (an `AtomicU32`, so this itself needs no lock); if
  that was the last pin, acquires `index`'s mutex and re-reads the pin
  count before telling the replacer the frame is evictable again. The
  re-read closes one TOCTOU: between the `fetch_sub` observing `0` and
  this call taking `index`'s mutex, another thread can win `pin_if_cached`
  on the same page and re-pin the frame, in which case the stale `0` must
  not be allowed to mark a now-pinned frame evictable. It does not close a
  second, more dangerous one on its own: in that same gap, a completely
  different thread's `acquire_free_frame` can evict this frame out from
  under its old page entirely, and the pin-count re-read alone can't tell
  a genuinely-still-unpinned frame from one that has since been evicted
  and is now `owned` by someone else's in-flight install
  (`docs/adr/0009-buffer-pool-frame-ownership.md`) - which is why the
  full condition also requires `!owned[idx]` before calling
  `set_evictable(frame_id, true)`, immediately followed by
  `frame_available.notify_one()` - the waiting rule above's other wakeup
  source, alongside `try_install`'s losing branch. Moves the
  `buffer_pool_pinned_frames` gauge down.
- `BufferPool::flush_pages(&self, pages) -> Result<(), StorageError>` -
  private. Refuses immediately with `StorageError::FlushPoisoned`, before
  touching the log, `dwb`, or the data file, if `flush_poisoned` is
  already set - see the poisoning paragraph below before assuming this
  check is optional. Otherwise emits a `trace!` naming the batch size
  before doing anything, and increments `dwb_batches_written_total` once
  the batch's double-write copy is durable (a nonzero rate here, per
  `docs/ROADMAP.md`'s M13 entry, is expected steady-state traffic, unlike
  `dwb_pages_restored_total` - see `recovery.MD` - which should be zero
  outside a crash). Flushes a batch of currently-dirty, resident pages to
  disk through the double-write buffer, latching each frame only long
  enough to clone a consistent snapshot of its bytes (never holding
  `index`'s mutex during any of this). Every entry in `pages` must already
  be dirty and safe from concurrent reassignment; this does not re-check
  either condition. Implements the write-ahead rule (the log must be
  durable to the batch's *highest* `page_lsn` before anything reaches the
  double-write buffer - not before each page individually, and not merely
  before the real file, which is what made a torn write unrecoverable
  before the double-write buffer existed) and the double-write protocol
  itself: write every page's image to `dwb` and sync it (so the batch is
  now recoverable even if nothing below has happened yet), only then write
  every page to its real location and sync that, then clear `dwb`. A crash
  at any point up to and including a synced `dwb.write_batch` leaves
  either a real page untouched by this batch, or a torn real page with an
  intact `dwb` copy `recovery::recover_double_write` can restore from.
  `flush_sequence` (the flush-sequence rule above) is taken before the
  per-page snapshot is even cloned and held through `dwb.clear_batch`
  returning (success or failure) and the dirty-clear loop right after it,
  with the authoritative `flush_poisoned` check made under that same lock
  - the check at the very top of this function is only a cheap early-out
  and is not by itself what makes poisoning safe.

  **Why a per-call-atomic `dwb` was not enough.** Before `flush_sequence`
  existed, `DoubleWriteBuffer`'s own internal mutex made each of
  `write_batch`/`clear_batch` individually atomic, but the three-step
  sequence around them was not, and `flush_pages` is reachable
  concurrently from `acquire_free_frame` (two evictions on different
  frames), `flush_page`, and `flush_all` alike. With `dwb`'s single header
  able to describe only one outstanding batch, T1's `write_batch([P])`
  followed by T2's `write_batch([Q])` silently discards P's backup before
  T1 has written P to its real location; if T1's real write then tears P
  and the process crashes, recovery finds Q's still-valid batch, restores
  nothing, and P is corrupt with no error ever raised anywhere. This was a
  real, reproduced failure - a `ChecksumMismatch` on reopen with no
  `dwb_pages_restored_total` warning ever logged - and required no I/O
  failure at all, only the ordinary interleaving two concurrent evictions
  already produce. `flush_sequence` closes it by making the sequence
  itself, not just its individual calls, exclusive.

  **Poisoning.** `dwb`'s slots are assigned by a batch's *position*
  (`DoubleWriteBuffer::offset_of_slot`), not per page id, so at most one
  batch's backup images can usefully exist on disk at a time - if this
  function fails anywhere from the real-page-write loop through
  `dwb.clear_batch` (a real write error, a sync failure, or the clear
  itself failing), the just-synced `dwb` batch is left in place,
  uncleared, exactly the state `recover_double_write` needs to repair a
  torn real page on the next open. A second, later `flush_pages` call -
  another eviction, a checkpoint, even just `Drop for Database`'s
  best-effort flush - has no way to know that batch is still needed, and
  writing a *new* one reuses the same slots: if that second write is
  itself interrupted, it can overwrite the first batch's still-valid
  backup with bytes that fail their own checksum, at which point
  `recover_double_write` reads "bad slot checksum" and - correctly, by
  its own rule - treats it as "the real write never started, nothing to
  restore," even though the real page from the *first* failure is
  actually torn and now has no way back. This was a real, reproduced bug
  (`problems.MD`), not a hypothetical: a `ChecksumMismatch` on reopen with
  no `dwb_pages_restored_total` warning ever logged. The fix is
  `flush_poisoned`: once this function fails in that window, it sets the
  flag (still under `flush_sequence`, so the set and the batch it
  describes become visible together) and every later call - on this pool,
  for the rest of this process,
  regardless of which pages it's asked to flush - fails fast with
  `StorageError::FlushPoisoned` instead of touching `dwb` again. Only a
  fresh `BufferPool` (i.e. reopening the database, running
  `recover_double_write` before any pool exists) clears it, which is also
  the only point it's safe to write a new batch again.

  Clearing `dirty_since_lsn` at the end is conditional, not unconditional:
  for each page, it re-takes the frame's `RwLock` (write half, so it is
  serialized against any `PageWriteGuard::write` in progress on that
  frame) and only clears the flag if the frame's *current* `page_lsn`
  still matches the snapshot's - i.e. nothing wrote to this page between
  the snapshot clone at the top of this function and now. `dirty_since_lsn`
  itself can't answer that question: `PageWriteGuard::write` only sets it
  when it was previously zero, so a page that was already dirty gets no
  new mark from a second write, and comparing the captured
  `dirty_since_lsn` before and after a flush would look unchanged either
  way. `page_lsn`, stamped on *every* write, is what actually moves - this
  is the fix for a real data-loss bug: the old unconditional clear
  erased a concurrent writer's change from the dirty page table with no
  record it was ever unflushed, so a later checkpoint's DPT would omit
  the page entirely and recovery would never redo it. Re-taking the write
  lock (rather than a lock-free compare-and-swap on a second atomic) is
  what makes the check-then-clear atomic with respect to a racing
  `PageWriteGuard::write`, which needs the same lock to mutate `page_lsn`
  in the first place.
- `BufferPool::frame_of(&self, page_id) -> Option<FrameId>` - private.
  `page_id`'s frame, or `None` if it isn't currently resident. Its only
  caller, `flush_page`, treats `None` as "already durable," not an error
  - see `flush_page` above.
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
- `BufferPool::truncate_log_below(&self, bound) -> Result<(), StorageError>` -
  wraps `LogManager::truncate_below`: deletes every whole write-ahead log
  segment strictly below `bound`. `txn::write_checkpoint` is the only
  caller, using a bound computed so nothing undo or redo can still reach
  is ever deleted (`wal.MD`'s truncation invariant,
  `docs/adr/0011-segmented-write-ahead-log.md`).
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
- `impl Drop for PageWriteGuard` - releases the write latch, then (in
  debug builds) removes its frame from `HELD_WRITE_GUARD_FRAMES`, then
  unpins.
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
