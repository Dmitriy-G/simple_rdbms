# buffer_pool_flush_races

Exercises `buffer::BufferPool::flush_pages` and the state it races
against, closing R-6 of `task.MD`: the `evicting` set
(`docs/adr/0010-buffer-pool-waits-for-a-frame.md`) and the
`page_lsn`-guarded dirty clear (`buffer.MD`) had no direct tests, only
the incidental coverage `buffer_pool.rs` and `buffer_pool_concurrency.rs`
gave them in passing. Unlike `buffer_pool_concurrency.rs`, which throws
many threads at ordinary fetch/evict traffic and trusts scale to find a
race, every test here drives one specific interleaving deliberately,
using either a one-shot failing device (`FailOnceDevice`) or a device
that pauses mid-write on command (`PausingDevice`) to hold `flush_pages`
open exactly where the property under test needs it held open. Ordered
cheapest first, matching `task.MD`'s R-6 list.

## Key Components

- `FailOnceDevice` - a `BlockDevice` wrapper whose `write_at` fails,
  without delegating to `inner` at all, on exactly the `fail_on_write`th
  call across the device's lifetime (1-based), then behaves normally on
  every call before and after. Used by
  `a_failed_eviction_does_not_leak_the_frame` to fail a real page write
  with no timing or threading required.
- `PausingDevice` - a `BlockDevice` wrapper whose `write_at` pauses the
  very first time it is asked to write `target_offset`: it signals
  `reached` (so the test knows the write is in flight), then blocks on
  `release` - bounded by `PAUSE_SAFETY_NET` so a test that forgets to
  release never hangs the whole suite - before delegating to `inner`.
  Every other write, and every later write to `target_offset`, passes
  straight through. `reached` and `release` are each locked with
  `unwrap_or_else(|poisoned| poisoned.into_inner())` rather than
  `.expect()`, since a panic on another thread poisoning either `Mutex`
  should not itself mask that panic with a second one here.
- `PAYLOAD_OFFSET` - `100`, outside `page::CHECKSUM_RANGE` (`0..4`) and
  `page::PAGE_LSN_RANGE` (`4..12`), so a test payload byte written here
  survives the checksum re-stamping every real disk write performs. Only
  `a_write_during_a_flush_leaves_the_frame_dirty` reads its payload back
  after a reopen, so it is the only test that needs this; the other
  tests write a marker byte at offset `0` purely to dirty the page; they
  never assert on stored content, only on `page_lsn` or on the pool's own
  bookkeeping.
- `open_pool`/`open_pool_with_data_device` - open a `BufferPool` over a
  temp directory's `test.db`/`test.db.dwb`/`test.db.wal`, the latter
  substituting a caller-supplied data device (a `FailOnceDevice` or
  `PausingDevice` wrapping a real `FileDevice`) for the fault injection.
- `a_failed_eviction_does_not_leak_the_frame` - a one-frame pool holds a
  page, then `new_page` forces its eviction into a `FailOnceDevice`
  primed to fail on the very first write. The pool is opened with a
  plain device first (`drop(open_pool(...))`) so the fault targets the
  eviction's page write and not `DiskManager::open_with_device`'s own
  header write to a freshly zero-length file. After the failed eviction,
  `pool.assert_frame_accounting()` checks `acquire_free_frame`'s
  `abort_eviction` path left no frame leaked. The failure lands after the
  double-write buffer's batch was synced but before it was cleared
  (`buffer.MD`'s "Poisoning" paragraph on `flush_pages` -
  `problems.MD`'s double-write-buffer-slot-reuse entry is what motivated
  it), so the pool must now refuse every further flush - fast, with
  `StorageError::FlushPoisoned`, never `BufferPoolExhausted` and never
  silently succeeding, since silently succeeding is exactly what would
  let a later flush overwrite the double-write backup
  `recovery::recover_double_write` still needs. `POOL_SIZE + 2` further
  `new_page` calls each assert that exact error, then
  `assert_frame_accounting()` runs again.
- `a_write_during_a_flush_leaves_the_frame_dirty` - writes a page,
  flushes it durably, then starts a background `flush_page` that pauses
  (via `PausingDevice`) with its snapshot of the page's bytes already
  taken but not yet on disk. While paused, the main thread writes a
  second, different value to the same page and releases the paused
  flush. Asserts `dirty_page_table()` still contains the page - the
  flush's `page_lsn`-guarded dirty clear (`buffer.MD`) must not clear
  dirty for a page whose `page_lsn` has moved past the snapshot it just
  wrote - then flushes and reopens to confirm the second write, not the
  first, is what is actually durable on disk.
- `a_fetch_during_an_eviction_flush_never_sees_the_stale_page` - a
  one-frame pool holds a dirtied page; a background thread's `new_page`
  can only proceed by evicting it, and that eviction's flush pauses
  mid-write. While paused, a `fetch_page_read` for the evicting page runs
  on its own thread (it must block rather than return, so it cannot share
  the main thread - releasing the pause from the same thread that is
  blocked on the fetch would deadlock). The eviction's flush is parked
  holding no locks (`buffer.MD`'s latch-ordering rule) but keeping the
  page in the pool's `evicting` set, so the fetch must block until the
  eviction resolves rather than either failing or returning a stale
  on-disk copy read before the eviction started. Releasing the pause lets
  both finish; the fetch's observed `page_lsn` is asserted equal to the
  `page_lsn` recorded right after the dirtying write - checked on
  `page_lsn`, not bytes, since the two threads' relative finishing order
  is not otherwise pinned down.
- `a_fetch_blocked_on_a_stuck_eviction_times_out_rather_than_hanging` -
  same setup as the previous test, but the pause is never released before
  the foreground `fetch_page_read` call, and the pool is built with
  `with_frame_wait_timeout(SHORT_FRAME_WAIT_TIMEOUT)`. `abort_eviction`
  and `complete_eviction` are the only paths that clear an entry from
  `evicting`, so a flush stuck between them leaves every later fetch of
  that page waiting; asserts the fetch fails with
  `StorageError::BufferPoolWaitTimedOut` and that it does so within a
  small multiple of `SHORT_FRAME_WAIT_TIMEOUT`, not merely eventually.
  Only then does it release the pause and join the eviction, so the test
  itself still terminates promptly.

## Usage Example

```rust
let (reached_tx, reached_rx) = mpsc::sync_channel::<()>(0);
let (release_tx, release_rx) = mpsc::sync_channel::<()>(0);
let data_device: Box<dyn BlockDevice> = Box::new(PausingDevice::new(
    Box::new(FileDevice::new(db_file)),
    target_offset,
    reached_tx,
    release_rx,
));
let pool = Arc::new(open_pool_with_data_device(dir.path(), POOL_SIZE, data_device)?.pool);

let handle = {
    let pool = Arc::clone(&pool);
    thread::spawn(move || pool.flush_page(page_id))
};
recv_within(&reached_rx, TEST_TIMEOUT, "the flush to reach the paused offset");
// ... race the paused flush from the main thread ...
release_tx.send(()).ok();
handle.join().unwrap()?;
```

Run via `cargo test -p storage --test buffer_pool_flush_races`. Every
test bounds its waits with `TEST_TIMEOUT` or `SHORT_FRAME_WAIT_TIMEOUT`
via `recv_within`, so a regression that reintroduces a hang fails fast
with a named cause instead of stalling the suite; per `CLAUDE.md`'s
concurrency discipline, check locally under `taskset -c 0,1` (or an
equivalent CPU-affinity constraint) before trusting a change to
`flush_pages` against this file.
