# ADR 0009: Buffer pool frames need an explicit ownership flag between acquisition and install

Date: 2026-08-28

Status: Accepted

## Context

`BufferPool::acquire_free_frame` hands back a frame that is unpinned,
absent from the page table and free list, and (on the eviction path)
already removed from the `Replacer` by `evict()`. Nothing marks that
frame as taken. The caller then does real work with no lock held - a
disk read in `fetch_frame`, an `append_log` in `new_page` - before
`try_install` finally records the frame's new page and pins it.

`unpin_frame` decrements a frame's pin count outside `index`'s mutex,
then takes the mutex and re-checks only that the pin count is still
zero before telling the replacer the frame is evictable again. That
re-check is stale by construction: it answers "is nobody currently
pinning this frame," not "is this frame still the same one I pinned."
If the frame was evicted in the gap between the decrement and the
mutex acquisition, the re-check still passes, and `set_evictable(frame,
true)` runs. `LruKReplacer::set_evictable` used to insert a fresh,
evictable entry whenever `frame_id` was absent, so this call resurrected
exactly the frame `evict()` had just removed - a frame the buffer pool
still considers "in flight" to some other caller's `try_install`.

Once resurrected, a second, unrelated `acquire_free_frame` call can
evict that same frame while its first owner is still reading into it.
Because `frame_page[idx]` is already `None` (cleared by the first
eviction), the second eviction removes nothing from the page table, and
both callers' `try_install` calls go on to succeed - one page id ends up
mapped to a frame that another, different page id is also mapped to,
and whichever caller wrote last wins the frame's actual bytes. This
reached CI as
`eight_threads_fetching_reading_and_releasing_never_see_torn_or_wrong_page_contents`
failing with "page PageId(6) (slot 5): expected marker 5, got 7," and
was rare enough that it did not reproduce locally without constraining
the test process to two CPUs.

## Decision

Make frame ownership explicit instead of implicit, as a `Vec<bool>`
(`owned`) inside `PoolIndex`, indexed by `FrameId`, guarded by the same
mutex as `page_table`/`frame_page`/`free_list`/`replacer` (a per-frame
`AtomicBool` was the other option on the table; a `Vec<bool>` under the
existing mutex was chosen because every read and write of it already
happens under that lock, so an atomic would have bought nothing but a
second, redundant synchronization mechanism to keep in sync with the
first):

- `acquire_free_frame` sets `owned[idx] = true` before releasing
  `index`'s mutex, on both the free-list and eviction paths, with a
  `debug_assert!` that the frame was not already owned - a frame that
  reaches here owned indicates some other path bypassed this handoff.
- `try_install` clears `owned[idx] = false` under `index`'s mutex, on
  the winning path right after publishing the frame's new mapping and on
  the losing path right before pushing the frame back onto the free
  list, with a `debug_assert!` that the frame *was* owned on entry.
- `unpin_frame`'s re-check now also requires `!owned[idx]` before calling
  `set_evictable(frame_id, true)`. This is the actual fix: an owned frame
  is mid-installation for someone else and must never re-enter the
  replacer, no matter how stale the unpinning caller's view of the pin
  count is.
- `LruKReplacer::set_evictable` no longer inserts an entry when
  `frame_id` is absent; it only ever modifies an entry `record_access`
  already created. A frame the replacer isn't tracking is either owned
  or sitting in the free list, and in both cases treating it as evictable
  is wrong regardless of which caller asked.
- `new_page` carries the same acquire-then-install shape and is covered
  by the same flag; nothing between its `acquire_free_frame` and
  `try_install` calls can return early today, so there is no leak path to
  close there, but the `debug_assert!`s above catch it immediately if a
  future edit introduces one - a frame stuck permanently `owned` shows up
  as `acquire_free_frame` panicking the next time the pool tries to
  reuse it, rather than as silent, slow pool exhaustion.

`BufferPool` gained a `#[cfg(any(test, feature = "test-util"))]` install
hook (`set_install_hook`/`clear_install_hook`), invoked with the
acquired `FrameId` between `acquire_free_frame` and `try_install` in
both `fetch_frame` and `new_page`, so a test can pause a thread there on
demand instead of hoping the OS scheduler happens to preempt it in the
same spot CI got unlucky in. `BufferPool::assert_invariants` is a
matching general-purpose check - every `page_table` entry's frame must
resolve back to the same page id in `frame_page`, and no frame id may
appear twice - intended to run at the end of any test that drives the
pool concurrently, in `crates/storage/tests/buffer_pool_concurrency.rs`
and elsewhere. `crates/storage/tests/buffer_pool_concurrency.rs`'s
`an_owned_frame_is_never_resurrected_by_a_racing_unpin` uses the hook to
park an installing thread mid-install while a pool of hammer threads
repeatedly pin and unpin the frame it just vacated, widening what was a
nanosecond-scale race into a window measured in tens of thousands of
loop iterations; against the pre-fix code this reliably trips the
`debug_assert!` in `acquire_free_frame` (a second thread successfully
re-acquiring a frame the first thread still owns) well before the
window closes.

## Consequences

`replacer.MD`'s statement of `set_evictable`'s contract changes: it now
only ever mutates an entry that already exists, never creates one. Any
future `Replacer` implementation must preserve that, since the buffer
pool's ownership tracking now depends on "absent from the replacer"
being a reliable signal that a frame is either owned or free, not merely
"nobody has called `set_evictable(true)` on it yet."

The `owned` vector adds one `bool` per pool frame and one branch to
`unpin_frame`'s already-locked slow path (the `remaining == 0` case,
which already takes the mutex); it adds nothing to the hot,
already-pinned-again path, since that returns before ever touching
`index`. The two new `debug_assert!`s are compiled out in release
builds, matching the precedent `docs/adr/0008-write-guard-reentrancy.md`
set for `HELD_WRITE_GUARD_FRAMES` - they exist to make a future
regression loud in development and CI, not to add a runtime check a
production build pays for.

The install hook and `assert_invariants` are general-purpose enough that
any future concurrency test against `BufferPool` should reach for them
first rather than inventing a new ad hoc synchronization mechanism per
test.
