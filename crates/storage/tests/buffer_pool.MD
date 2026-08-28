# buffer_pool

Buffer pool behavior: eviction under pressure, dirty data surviving a
flush and refetch, pinning every frame forcing a clean error instead of a
panic, the pin-count protocol itself (a page stays resident for as long
as *any* `PageGuard` on it is alive, and only becomes evictable once the
last one drops), and the write-guard rules `buffer.MD`'s latch-ordering
section documents: at most one write guard per page per thread, any
number of read guards, and a write guard blocking behind a live read
guard rather than racing it.

Three of these tests used to take a *second write guard* on an
already-write-guarded page to prove the pin-count property, back when
`PageGuard` was purely a pin with no latch attached. Once M10.2 gave each
frame its own `RwLock` (`buffer.MD`), that pattern became a same-thread
self-deadlock: `std::sync::RwLock` is not reentrant, so a second
`fetch_page` on a page this thread already holds a write guard to blocks
forever on itself, and would have hung `cargo test --workspace` rather
than failing it. `second_live_read_guard_keeps_page_pinned_under_eviction_pressure`
and `pin_count_reaches_zero_only_after_the_last_read_guard_drops` were
rewritten to establish the page's content through one write guard,
**drop it**, and take every additional live guard as a `fetch_page_read`
instead - keeping the exact property under test (a live guard keeps its
page unevictable) while only changing the latch mode, since the pin-count
mechanics `unpin_frame` implements don't care whether a pin came from a
read or a write guard.

## Key Components

- `MARKER_OFFSET` - `100`, an arbitrary offset past the reserved checksum
  and `page_lsn` prefix (`0..12`), used by these tests as a one-byte
  marker location. What byte of the page it is doesn't matter to the
  buffer pool; it only matters that it isn't the `page_lsn` field, which
  `PageGuard::write` overwrites on every write.
- `open_pool(pool_size)` - opens a `BufferPool` of `pool_size` frames over
  a fresh temp-dir database, WAL, and double-write buffer, with an LRU-K(2)
  replacer.
- `open_pool_lru(pool_size)` - like `open_pool`, but with `k = 1` so the
  replacer's victim choice is plain LRU (oldest access wins) instead of
  LRU-K's "prefer evicting frames with less than `k` accesses" heuristic,
  which otherwise makes eviction order hard to predict in a short-lived
  test.
- `apply_pressure(pool, n)` - allocates and immediately releases `n`
  throwaway pages, to apply eviction pressure on whatever else is
  currently unpinned in the pool.
- `evicts_under_pressure_and_dirty_data_survives_flush_and_refetch` -
  allocates 10 pages into a 4-frame pool, so the first page written was
  necessarily evicted at least once; asserts its dirty byte was flushed
  to disk before eviction and comes back correctly on refetch after an
  explicit `flush_all`.
- `fetch_errors_rather_than_panics_when_every_frame_is_pinned` - with
  every frame in a 2-frame pool pinned *by this same thread*, a third
  `new_page` call must return `StorageError::BufferPoolExhausted`
  immediately, not panic and not block. Since
  `docs/adr/0010-buffer-pool-waits-for-a-frame.md`, `acquire_free_frame`
  waits for a frame to free up rather than failing on the spot - this
  test's every-frame-pinned-by-the-caller shape is specifically the one
  case waiting can never resolve (a guaranteed self-deadlock), which is
  why it still fails fast rather than hanging for 30 seconds before
  timing out with `BufferPoolWaitTimedOut` instead.
- `second_live_read_guard_keeps_page_pinned_under_eviction_pressure` -
  proves `PageGuard` is the sole owner of its own pin, not a shared handle
  to "the page is pinned or not": writes the page's marker through one
  write guard and drops it, takes two `fetch_page_read` guards on the
  same page, and confirms releasing one must not release the pin the
  other still holds, verified by applying heavy eviction pressure and
  checking the page survives.
- `pin_count_reaches_zero_only_after_the_last_read_guard_drops` -
  establishes the page's marker through one write guard, drops it, then
  takes `N = 4` `fetch_page_read` guards on the same page and drops them
  one at a time; after each drop except the last, confirms the page
  survives a fresh round of eviction pressure, and only after the final
  guard drops does pressure succeed in reclaiming its frame.
- `write_guard_blocks_until_concurrent_read_guard_drops` - the read-guard
  rule's other half: while this thread holds a `fetch_page_read` guard on
  P, a second thread's `fetch_page(P)` must block rather than racing it -
  proven, not just asserted, by having that thread report through an
  `mpsc` channel and confirming a `recv_timeout` first *times out* (the
  write is still blocked) and only then succeeds, immediately after the
  read guard is dropped. Unlike the self-deadlock this file is otherwise
  about, this is ordinary, correct blocking between two different
  threads - the write guard is never granted to the same thread that
  holds the read guard - so it belongs here as the positive case the
  single-thread rule doesn't cover: reads never block each other, and a
  write blocks behind them without erroring or deadlocking. A `Barrier`
  starts the two sides together so the timeout check can't pass merely
  because the writer thread hadn't started yet.
- `a_second_write_guard_on_the_same_page_from_one_thread_panics` -
  `#[cfg(debug_assertions)]`, `#[should_panic(expected = "self-deadlock")]`:
  holds a `new_page` write guard and calls `fetch_page` on the same page
  from the same thread without dropping it first, proving
  `buffer::BufferPool`'s debug-only reentrancy detector
  (`buffer.MD`) fires instead of letting the thread park on its own
  latch forever. Gated on `debug_assertions` because the detector itself
  is compiled out in release - the test would otherwise fail to panic in
  a release run, not because the bug reappeared, but because there is
  nothing left in that build to catch it.
- Every test above except this last one (which panics before returning)
  finishes with `pool.assert_frame_accounting()` -
  `buffer::BufferPool`'s general-purpose page-table/`frame_page`/
  `free_list`/`owned` consistency check (`buffer.MD`).

## Usage Example

```rust
let (pool, _dir) = open_pool(4)?;
let (page_id, mut guard) = pool.new_page(common::TxnId(0))?;
guard.write(common::TxnId(0), MARKER_OFFSET, &[7])?;
drop(guard);
pool.flush_all()?;
```

Run via `cargo test -p storage --test buffer_pool`, exercising
`storage::buffer::BufferPool` end-to-end against real temp-dir files
rather than mocking the disk manager or replacer.
