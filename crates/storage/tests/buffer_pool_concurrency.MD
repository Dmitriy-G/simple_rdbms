# buffer_pool_concurrency

Proves the M10.1 concurrency refactor's latching actually holds under
real, simultaneous access - not the type-level `Send + Sync` proof
(`crates/engine/tests/send_sync.MD`), but a genuine multi-threaded
stress test of `BufferPool::fetch_page_read`/pin/eviction. Deliberately
does not exercise transaction isolation, which M10.1 does not add (see
`buffer.MD`'s latch-ordering rule); it only exercises the frame-level
`RwLock` and pin-count bookkeeping that isolation will eventually sit on
top of.

## Key Components

- `MARKER_OFFSET` - `100`, the same arbitrary marker byte offset
  `buffer_pool.MD`'s tests use.
- `POOL_SIZE` / `PAGE_COUNT` - a 4-frame pool against 20 pages, so every
  thread's fetches force ongoing eviction and reinstallation of frames
  throughout the run, not just a one-time warm-up.
- `THREAD_COUNT` / `ITERATIONS_PER_THREAD` - 8 threads, 200 fetches each.
- `eight_threads_fetching_reading_and_releasing_never_see_torn_or_wrong_page_contents` -
  writes a distinct one-byte marker into each of `PAGE_COUNT` pages and
  flushes, then spawns `THREAD_COUNT` threads that each loop
  `ITERATIONS_PER_THREAD` times, computing a page index from a fixed
  per-thread stride (`thread_index * 37 + i`, an arbitrary odd stride
  relative to `PAGE_COUNT` chosen only so each thread's access sequence
  doesn't degenerate into every thread reading the same page in lockstep)
  so different threads are very likely mid-fetch on different pages at
  the same instant, calling `fetch_page_read`, and asserting the byte at
  `MARKER_OFFSET` still matches that page's original marker. A torn read,
  a frame reinstalled with the wrong content while still pinned, or a pin
  count race that let a live frame be evicted would all surface here as a
  mismatched marker (or a panic) in some thread; a clean join of all
  eight is the proof this doesn't happen. Every worker returns
  `Result<(), String>` instead of panicking directly, so a failure names
  the exact page and thread rather than aborting the process before the
  other threads' state can be inspected.

## Usage Example

```rust
let pool = Arc::new(pool);
let page_ids = Arc::new(page_ids);
let handle = {
    let pool = Arc::clone(&pool);
    let page_ids = Arc::clone(&page_ids);
    thread::spawn(move || {
        let guard = pool.fetch_page_read(page_ids[0])?;
        assert_eq!(guard.page().data()[MARKER_OFFSET], 0);
        Ok::<_, String>(())
    })
};
handle.join().expect("worker thread panicked")?;
```

Run via `cargo test -p storage --test buffer_pool_concurrency -- --test-threads=1`,
the dedicated CI step `.github/workflows/ci.yml` adds specifically for
this file - the assertion this test makes is about eight threads racing
each other, which starving them of real cores by running them alongside
every other test binary's own parallelism would make slower to fail and
easier to falsely pass.
