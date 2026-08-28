# buffer_pool_concurrency

Proves the M10.1 concurrency refactor's latching actually holds under
real, simultaneous access - not the type-level `Send + Sync` proof
(`crates/engine/tests/send_sync.MD`), but a genuine multi-threaded
stress test of `BufferPool::fetch_page_read`/pin/eviction, and (M10.2)
of the double-checked install (`buffer::BufferPool::try_install`,
`buffer.MD`) that closes the double-fetch race two threads missing on
the same page at once used to hit. Deliberately does not exercise
transaction isolation, which M10.1 does not add (see `buffer.MD`'s
latch-ordering rule); it only exercises the frame-level `RwLock`,
pin-count bookkeeping, and page-table publication that isolation will
eventually sit on top of.

## Key Components

- `MARKER_OFFSET` - `100`, the same arbitrary marker byte offset
  `buffer_pool.MD`'s tests use.
- `THREAD_COUNT` / `ITERATIONS_PER_THREAD` - 8 threads, 200 fetches each.
- `POOL_SIZE` / `PAGE_COUNT` - `THREAD_COUNT + 1` frames (9) against 20
  pages: still small enough relative to `PAGE_COUNT` that every thread's
  fetches force ongoing eviction and reinstallation of frames throughout
  the run rather than a one-time warm-up, but never smaller than the
  number of threads. Each thread only ever has one page pinned at a
  time, so `THREAD_COUNT` is a hard ceiling on how many distinct frames
  every thread could simultaneously demand at once; sizing the pool
  below that ceiling doesn't add interesting contention; it just means
  the pool can be legitimately out of evictable frames at the exact
  moment every thread's stride (`thread_index * 37 + i`, see below)
  happens to land on `THREAD_COUNT` distinct, not-yet-resident pages in
  the same instant - which the stride does at `i = 0` deterministically,
  not by rare bad luck (`thread_index * 37 % 20` is already 8 distinct
  values for `thread_index` in `0..8`). A pool this small is a real
  capacity shortfall for that access pattern, not a `BufferPool` bug -
  `StorageError::BufferPoolExhausted` is the documented, correct response
  to more concurrent distinct-page demand than frames exist - so the fix
  belongs in this test's sizing, not in `try_install`/`acquire_free_frame`.
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
  other threads' state can be inspected. Only ever exercises
  `fetch_page_read` on pages that are, at worst, already resident, so it
  says nothing about the double-fetch race below - that needs a genuine
  cache *miss* shared by two threads at once, which is what the next two
  tests force deliberately instead of leaving to scheduling luck.
- `COUNTER_OFFSET` - `200`, distinct from `MARKER_OFFSET`, the byte
  offset the read-modify-write test's four-byte counter lives at.
  `read_counter(data)` reads it back as a little-endian `u32`.
- `RMW_PAGE_COUNT` / `RMW_THREAD_COUNT` / `RMW_POOL_SIZE` / `RMW_ROUNDS` -
  3 target pages contended by 8 threads (more threads than pages, so a
  page's counter is fought over every round), `RMW_THREAD_COUNT + 1`
  frames, and 200 rounds. The pool must hold at least `RMW_THREAD_COUNT`
  frames, not just `RMW_PAGE_COUNT`: `try_install`'s whole design lets
  every racing thread claim its own frame and read the page *before*
  any of them checks whether it already lost, so in the worst case all
  eight threads competing this round each hold a distinct frame
  transiently, even though only `RMW_PAGE_COUNT` of them ever end up
  actually installed - a smaller pool hits a real, uninteresting
  `BufferPoolExhausted` from that transient demand rather than the race
  this test exists to catch.
- `concurrent_read_modify_write_cycles_never_lose_an_update` - allocates
  `RMW_PAGE_COUNT` pages with a zeroed counter and flushes them, then
  runs `RMW_ROUNDS` rounds. Each round first forces every target page out
  of cache - single-threaded, before any racing starts - by allocating
  `RMW_POOL_SIZE` throwaway filler pages, which is enough that the LRU-K
  replacer necessarily evicts everything resident to make room; then a
  fresh `Barrier` releases `RMW_THREAD_COUNT` threads together, each
  computing its target page from `(thread_index + round) % RMW_PAGE_COUNT`
  and running exactly one `fetch_page` / read-counter / increment /
  `write` / drop cycle before the round ends. Because the target was just
  forced cold and several threads are released simultaneously, multiple
  threads reliably *miss* on the same page at once - the exact window
  `try_install`'s double-checked insert has to arbitrate. After all
  rounds, the pool is flushed, dropped, and reopened from the same files
  (mirroring `crates/storage/tests/recovery_logging.rs`'s reopen
  pattern) so the assertion reads back durable state rather than
  whatever happens to still be resident; each page's counter must equal
  the number of rounds that targeted it, computed independently of the
  worker loop. Before the M10.2 fix, a lost race let two threads each
  install their own frame for the same page and write their increment
  into it, and only one of those frames stays reachable through the page
  table afterward - the other's increment is gone forever, so the
  reopened counter comes up short. A single missed race is enough to
  make this fail, since the discrepancy persists through every later
  round rather than only showing up transiently.
- `RACE_POOL_SIZE` / `RACE_ITERATIONS` - a 2-frame pool (exactly enough
  for two racing threads) and 200 trials.
- `two_threads_racing_a_cold_page_install_yields_exactly_one_frame` -
  allocates `RACE_ITERATIONS + RACE_POOL_SIZE` pages up front and flushes
  them; because the pool holds only `RACE_POOL_SIZE` frames, by the time
  allocation finishes only the last `RACE_POOL_SIZE` pages are still
  resident, leaving every one of the first `RACE_ITERATIONS` pages
  already evicted and never touched again until its trial. For each of
  those pages in turn: a fresh two-party `Barrier` releases two threads
  together, both calling `fetch_page_read` on the same guaranteed-cold
  page, then - after both join - asserts `BufferPool::frame_count_for`
  (`buffer.MD`) reports exactly one resident frame for it. Unlike the
  read-modify-write test, this doesn't need the *content* to be wrong to
  fail; it directly checks the invariant `try_install` exists to
  maintain (one page, one frame) without waiting for a lost update to
  manifest as a wrong number somewhere downstream.

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

```rust
let barrier = Barrier::new(2);
thread::scope(|scope| {
    for _ in 0..2 {
        scope.spawn(|| {
            barrier.wait();
            pool.fetch_page_read(page_id).unwrap();
        });
    }
});
assert_eq!(pool.frame_count_for(page_id), 1);
```

Run via `cargo test -p storage --test buffer_pool_concurrency -- --test-threads=1`,
the dedicated CI step `.github/workflows/ci.yml` adds specifically for
this file - every test in it makes an assertion about real threads racing
each other, which starving them of real cores by running them alongside
every other test binary's own parallelism would make slower to fail and
easier to falsely pass. All three tests are timing-dependent by nature
(a `Barrier` maximizes the odds of hitting each race window, but cannot
guarantee it on every single run the way a deterministic test would);
before trusting a change to this file or to `buffer::BufferPool`, run
this binary in a loop (at least 50 times, per `task.MD`'s M10.2 entry)
rather than accepting one green run as proof.
