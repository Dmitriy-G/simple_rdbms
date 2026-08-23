# buffer_pool

Buffer pool behavior: eviction under pressure, dirty data surviving a
flush and refetch, pinning every frame forcing a clean error instead of a
panic, and the pin-count protocol itself (a page stays resident for as
long as *any* `PageGuard` on it is alive, and only becomes evictable once
the last one drops).

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
  every frame in a 2-frame pool pinned, a third `new_page` call must
  return `StorageError::BufferPoolExhausted`, not panic.
- `second_live_guard_keeps_page_pinned_under_eviction_pressure` - proves
  `PageGuard` is the sole owner of its own pin, not a shared handle to
  "the page is pinned or not": releasing one of two live guards on the
  same page must not release the pin the other still holds, verified by
  applying heavy eviction pressure and checking the page survives.
- `pin_count_reaches_zero_only_after_the_last_guard_drops` - takes `N = 4`
  guards on the same page, drops them one at a time, and after each drop
  except the last, confirms the page survives a fresh round of eviction
  pressure; only after the final guard drops does pressure succeed in
  reclaiming its frame.

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
