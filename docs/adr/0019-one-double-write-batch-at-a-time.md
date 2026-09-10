# ADR 0019: One double-write batch at a time, and what a page fetch may wait for

Date: 2026-09-09

Status: Accepted

## Context

**This section describes `crates/storage` as it stood before this
decision was implemented**; the Decision below is what the code does now,
so its line numbers are omitted — a historical account is not a map of
the current tree, and a citation that resolves to something else is worse
than none.

`BufferPool::flush_pages` took `BufferPool.flush_sequence` and held it to
the end of the function: the per-page snapshot clone, the log flush to
the batch's highest `page_lsn`, `dwb.write_batch` and its `sync_all`,
every real `write_page`, `disk_manager.sync()`, `dwb.clear_batch` and its
`sync_all`, and the loop that clears each frame's `dirty_since_lsn`.
Three device syncs, none of them bounded by anything. `acquire_free_frame`
called it inline when the victim it picked was dirty, so it sat on the
ordinary page-fetch path, not on a background one.

This looks exactly like the defect ADR 0014, ADR 0016 and ADR 0018 each
fixed one layer above: a lock taken for a short logical operation and held
across I/O whose duration nothing bounds. Three stall investigations have
now reached it and stopped — ADR 0018's closing paragraphs record the
third, where a probe `SELECT` in
`crates/engine/tests/dispatch_never_blocks.rs`'s
`a_stalled_rollback_does_not_block_an_unrelated_session` timed out with
the device parked and the test had to be narrowed to `BEGIN`/`COMMIT`
before it could be committed. Each investigation noted the serialization,
decided it was one layer below its own subject, and moved on. Nothing in
`docs/adr/` says whether it is a defect, so the fourth investigation would
start from the same place.

The reason the fourth reading is tempting and wrong is that the earlier
three share a shape this one does not. There, the lock protected *logical*
state — the catalog, the transaction manager — and the I/O underneath it
was incidental: the flush did not need the mutex, it merely happened to be
holding it. Here the lock protects the *physical* staging area the I/O is
about. Two flushes cannot proceed at once against one double-write buffer
no matter how the lock is arranged, so there is no narrower critical
section to find; the question is not "how do we not hold this across an
`fsync`" but "what does a caller that did not come here to write have to
wait for".

## Decision

### 1. Exactly one double-write batch is in flight at a time, by design

`DoubleWriteBuffer` is one file with **one header**
(`crates/storage/src/dwb.rs:128-168`) whose entries are the only record of
which slots recovery may trust, and slots are addressed by a batch's
*position* (`offset_of_slot`, `crates/storage/src/dwb.rs:56-58`), not per
page id. At most one batch's backup images can usefully exist on disk, so
at most one batch may be in flight. Overlapping them is not slower, it is
wrong: T1's `write_batch([P])` followed by T2's `write_batch([Q])`
discards P's backup while P's real write is still in flight, and a tear on
that write is then unrecoverable with no error raised anywhere. That was a
reproduced failure, not a hypothesis — `crates/storage/src/buffer.MD`'s
"Why a per-call-atomic `dwb` was not enough" records it and
`crates/storage/tests/dwb_batch_exclusion.rs` is its regression test.

Nor can the exclusion be narrowed to the `write_batch`/`clear_batch` pair.
The snapshot clone at the top of `flush_pages` must be serialized against
another flush's clone-and-write of the same frame, or an older snapshot's
bytes land after a newer one's with the frame already marked clean.

So: **`BufferPool.flush_sequence` is part of ADR 0005's durability
boundary, not a lock to be narrowed.** It is held across three `fsync`s
deliberately. A change that narrows or removes it replaces this ADR and
ADR 0005 together; it is not a local fix in `buffer.rs`.

### 2. Partitioning the double-write buffer is rejected for now

The obvious way to get N concurrent flushes is N independent regions with
N headers. Rejected, for four reasons, none of which is "it would not
work":

- `recovery::recover_double_write` would have to scan and trust N headers
  instead of one, and the on-disk magic `FDBDWB02`
  (`crates/storage/src/dwb.rs:13`) becomes `03` with a format migration.
- The crash-injection sweep's fail points multiply with the number of
  regions, and that sweep is already the most expensive gate in the
  repository.
- Capacity per region falls. ADR 0005 sized `DbConfig::dwb_capacity`
  (default 64) so that `flush_all` amortizes three syncs over many pages;
  four regions make that four round trips of 16 pages, which is worse for
  the checkpoint path in exchange for being better for the eviction path.
- The win is bounded by the device. Concurrent batches do not make one
  device faster; on a queue-depth-1 device they only interleave.

Revisit only on a measurement showing flush *throughput* is the limit —
that is, a device with real internal parallelism saturated by one batch at
a time. Flush *latency*, which is what a stalled-write test exhibits, is
not evidence for partitioning: nothing about N regions makes the one write
a session is waiting on complete sooner.

### 3. What the contract owes a caller instead

"One batch at a time" is not a licence to park arbitrary sessions for an
unbounded time. Two obligations follow, and together they are the whole of
what `flush_sequence` may cost a caller.

**(a) A fetch that did not come to write does not queue behind one that
did — while the pool has a clean frame to give it.** Half of this already
holds: a clean victim is evicted with no flush and no
`flush_sequence` at all (`crates/storage/src/buffer.rs:559-566`). What
does not hold is victim *choice*. `LruKReplacer::evict_where`
(`crates/storage/src/replacer.rs:60-80`) ranks purely by LRU-K recency, so
a reader can be enrolled into another session's device queue while clean
evictable frames sit in the pool untouched — and worse, a page read once
to warm it has fewer than `k` recorded accesses and is therefore picked
*first*, so warming a page does not avoid the eviction, it guarantees one.

Decision: **when a flush is already in flight, eviction prefers a clean
victim; when none is, victim choice is unchanged.** Conditioning on
contention rather than always preferring clean is deliberate: with no
flush in flight, evicting a dirty victim costs one flush that starts
immediately and queues behind nothing, so the LRU-K choice is still the
right one and hit rate is untouched in every case where the preference
would cost something. Residency skew stays bounded in the contended case
too, because `checkpoint_and_flush` calls `flush_all`
(`crates/engine/src/runtime.rs:675`) and empties the dirty set on the
checkpoint threshold.

**(b) A fetch that does need a flush waits for it, but not forever.**
ADR 0010 states that `fetch_page`/`fetch_page_read`/`new_page` may block
"for up to `frame_wait_timeout`". That was false when this was written:
`acquire_free_frame` computed a deadline
(`crates/storage/src/buffer.rs:486`) and honoured it throughout its own
wait loop, then left the loop and called `flush_pages`, which waited on
`flush_sequence` with no deadline at all.

Decision: **the eviction path passes its remaining budget into the flush,
and a wait that exhausts it returns `StorageError::BufferPoolWaitTimedOut
{ waited_ms }`** — the same error the frame wait itself returns
(`crates/storage/src/buffer.rs:535`), because it means the same thing to a
caller: the pool could not give you a frame inside the window. This
restores ADR 0010's stated bound rather than inventing a new one, and no
new SQLSTATE is introduced. Whether that error class should be retryable —
it maps to `OUT_OF_MEMORY` (`crates/common/src/error.rs:151`) and
`Error::is_retryable` (`:228-236`) is false for it — is one open question
about one error, and both of its cases answer it the same way; it is not
settled here.

The bound applies to the eviction path only. Every other caller of
`flush_pages` — `flush_page`, `flush_all`, the checkpoint's page-0 flush
(`crates/txn/src/checkpoint.rs:45`), recovery, and `Drop`'s best-effort
flush — came to write and keeps waiting indefinitely. Failing them on a
timeout would turn a slow device into a durability event, which is the
opposite trade. What the bound protects is the worker pool: statements run
on a fixed eight-thread pool (`engine::runtime`), and a thread parked
indefinitely inside an eviction is one of eight gone. That is the same
argument ADR 0012 made for bounded lock waits, one layer down.

### 4. What none of this buys, stated so it is not looked for again

A session that must write to a stalled device waits for that device. In
`dispatch_never_blocks.rs`'s rollback test the pool is 8 frames
(`crates/engine/tests/dispatch_never_blocks.rs:257`) and 40 inserts of a
1800-byte row (`:283-286`) leave every one of them dirty, so the probe
`SELECT` genuinely needs a page written before it can read one. Part 3(a)
does not change that and is not meant to. The strong-form probe P-71
wanted — a real `SELECT` inside the timed window — is meaningful only with
a pool sized so that a clean evictable frame exists. That test was written
one layer down instead, as
`crates/storage/tests/buffer_pool_flush_races.rs`'s
`an_eviction_prefers_a_clean_victim_while_a_flush_is_in_flight`: at engine
level the dirty set is a consequence of whatever DDL, inserts and
checkpoints the test happened to run, so keeping one frame clean through a
`ROLLBACK`'s undo would be a tuned test rather than a deterministic one,
and tuning a concurrency test until it reproduces is the practice
`CLAUDE.md` bans outright.

The honest claim, and the one the tests should assert, is: **an unrelated
read stalls behind a device write only when every evictable frame in the
pool is dirty.**

## Consequences

Parts 1, 2 and 4 were statements about the tree as it already stood; part
3 was work, and it has since landed. `flush_sequence` is a `Mutex<bool>`
paired with a `flush_available` `Condvar`, since `std::sync::Mutex` has no
timed acquire, taken through `BufferPool::begin_flush` and released by the
`FlushLease` it returns; `flush_pages_before` carries the optional
deadline and `flush_pages` is the `None` case every other caller uses.
`Replacer::evict_where` takes a predicate and leaves rejected candidates
tracked, rather than the pool evicting and re-offering what it does not
want — `evict` stops tracking the frame it returns, and with it the LRU-K
history that made it a good victim — and `evict` is now a provided method
calling it. The advisory `flush_in_flight: AtomicBool` is what the victim
choice reads, because the latch-ordering rule forbids touching
`flush_sequence` under the pool's `index` mutex. `buffer_pool_flush_waits_total`,
`buffer_pool_flush_wait_seconds` and
`buffer_pool_clean_victims_preferred_total` are the new signals, and
`BufferPool::flush_wait_count` is their `test-util` mirror — which is what
lets a responsiveness test assert on a counter instead of a clock.

The eviction path gains a failure mode it did not have: under a device
slow enough to exhaust `frame_wait_timeout` (30s by default), a statement
now fails with `BufferPoolWaitTimedOut` where before it blocked. That is
the intended trade — a returned error frees a worker thread and a blocked
one does not — but it means a deployment on a device with multi-second
write latency will see statement failures that previously presented as
hangs, and `frame_wait_timeout` is the knob for it.

What becomes easier is the next stall investigation. The test that
separates this ADR's case from ADR 0014's, 0016's and 0018's is: *what
else does this lock guard?* A lock protecting logical state that the I/O
is incidental to is a defect and gets split. A lock protecting the
physical resource the I/O is about is the design, and the obligations it
owes are the two in part 3 — do not queue a caller who does not need the
resource, and bound the wait of one who does.
