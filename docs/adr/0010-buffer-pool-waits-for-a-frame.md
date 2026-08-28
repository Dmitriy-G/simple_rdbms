# ADR 0010: The buffer pool waits for a free frame instead of failing, and dedupes concurrent misses per page

Date: 2026-08-29

Status: Accepted

## Context

`BufferPool::acquire_free_frame` returned `StorageError::BufferPoolExhausted`
the instant no frame was free or evictable. For a server handling several
connections at once, "every frame is momentarily busy" is an ordinary,
transient condition, not a reason to fail a client's statement - no
production database surfaces a hard error just because another session's
statement is using the pool right now. This was the last piece of M10.1
left unaddressed: the buffer pool's frame count was still effectively a
hard ceiling on concurrency rather than a resource threads could wait on.

Restoring real contention in `crates/storage/tests/buffer_pool_concurrency.rs`
(shrinking `POOL_SIZE`/`RMW_POOL_SIZE` below the thread counts, as this ADR's
fix requires in order to be exercised at all) surfaced a second, unrelated
bug once threads could genuinely wait rather than fail fast: a lost update
in `concurrent_read_modify_write_cycles_never_lose_an_update`, reproducible
within seconds under the smaller pool. Two threads racing a cold miss on
the same page each independently read it from disk (a known, previously
"safe but wasteful" property - see the `TODO(M10.2)` this ADR removes from
`fetch_frame`) and race in `try_install`, which lets exactly one of them
publish its copy. That race was safe only because the window between "read
from disk" and "try to install" used to be a few instructions wide. Once
`acquire_free_frame` can legitimately block for real time waiting on a
frame, that window can now span an entire write-then-evict cycle: if the
winner installs, gets written to, and is evicted (flushing its update)
before the loser's `try_install` call finally runs, the loser finds the
page table empty again and installs its own, now-stale disk read as if it
were current - silently overwriting the winner's update. This is not a
frame-ownership bug like `docs/adr/0009-buffer-pool-frame-ownership.md`'s -
every frame involved is correctly owned and pinned throughout - it is a
staleness bug in the assumption that a second reader's copy can never
become outdated before it gets used.

## Decision

**Wait instead of failing.** `BufferPool` gained a `Condvar`
(`frame_available`) paired with the existing `index` mutex and a
`frame_wait_timeout: Duration` (default `DEFAULT_FRAME_WAIT_TIMEOUT`, 30s,
overridable per pool via the builder method `with_frame_wait_timeout`).
`acquire_free_frame` is now a loop: try the free list, try evicting, and if
neither has anything, wait on the condvar up to the remaining timeout and
retry. `unpin_frame` and `try_install`'s losing branch each `notify_one`
when they make a frame available again. A genuine timeout returns the new
`StorageError::BufferPoolWaitTimedOut { waited_ms }`
(`common::Error::BufferPoolWaitTimedOut`, `SqlState::OUT_OF_MEMORY`,
distinct from `BufferPoolExhausted` since it represents a different
condition to a caller deciding whether to retry: exhaustion right now vs.
exhaustion that persisted for the whole timeout window). `acquire_free_frame`
still fails immediately, without waiting, for the one case waiting cannot
fix: every frame already pinned by the *calling* thread. A
`#[cfg(debug_assertions)]` thread-local pin count
(`PINNED_FRAME_COUNTS`, mirroring `HELD_WRITE_GUARD_FRAMES` from
`docs/adr/0008-write-guard-reentrancy.md`) detects this and returns
`BufferPoolExhausted` immediately - a guaranteed self-deadlock should say
so at once, not after 30 seconds.

**Dedupe concurrent misses on the same page.** `fetch_frame`'s miss path no
longer lets every racing thread read the page from disk independently.
`PoolIndex` gained a `loading: HashSet<PageId>` and `BufferPool` a second
`Condvar` (`page_installed`) on the same mutex. A missing thread first
tries to insert `page_id` into `loading`; if it's already there, the
thread waits on `page_installed` (bounded by the same
`frame_wait_timeout`) and re-checks the page table each time it wakes -
either the page is now resident (ordinary cache hit) or `loading` is free
and this thread claims it. Only the one thread that wins the claim reads
disk and calls `try_install`; every other misser waits for that result
instead of racing its own stale copy against it. The claim is released
(and every waiter woken via `notify_all`, since more than one may be
waiting on the same page) on *every* exit from the load - including
`acquire_free_frame`'s own timeout - which is why the load is wrapped in a
closure rather than using `?` directly against `fetch_frame`'s return:
an early return through `?` before this ADR's fix shipped left `loading`
permanently set, hanging every subsequent fetch of that page id forever.
Fixing this closed a latent, pre-existing leak in the same neighborhood:
a disk-read failure (e.g. a checksum mismatch) used to leave its
just-acquired frame permanently marked `owned` and never returned to the
free list; `release_owned_frame` now runs on that path too.

**Frame accounting became a stronger, single check.** The test-only
invariant checker from `docs/adr/0009-buffer-pool-frame-ownership.md`
(`assert_invariants`) is renamed `assert_frame_accounting` and extended to
also assert every free-list frame is unowned with no `frame_page` entry,
and every owned frame is absent from both the page table and the free
list - the fuller set of invariants `PoolIndex`'s four collections
(`page_table`, `frame_page`, `free_list`, `owned`) must maintain together.
It now runs at the end of every test in `buffer_pool_concurrency.rs` and
`buffer_pool.rs`.

## Consequences

`crates/storage/tests/buffer_pool_concurrency.rs`'s `POOL_SIZE`/
`RMW_POOL_SIZE` are now literal `4`s rather than `THREAD_COUNT + 1`/
`RMW_THREAD_COUNT + 1` - a pool smaller than the thread count contending
for it is now the point of those tests, not an oversight the previous,
fail-fast pool forced them to avoid. Both tests assert every thread
completes every iteration despite that undersizing, which is only a
meaningful property once waiting works.

Any call site that used to treat `BufferPoolExhausted` as effectively
immediate must now expect `fetch_page`/`fetch_page_read`/`new_page` to
block for up to `frame_wait_timeout` before returning either result. No
caller in this codebase currently holds a lock across one of these calls
that a waiting thread could need released, but a future one must avoid
that shape, or a two-thread real deadlock (not the self-deadlock case this
ADR detects) becomes possible.

`fetch_frame`'s single-loader change also delivers the efficiency half of
the `TODO(M10.2)` comment it replaces: a cold page shared by many
concurrent readers is now read from disk exactly once, not once per
racing thread.
