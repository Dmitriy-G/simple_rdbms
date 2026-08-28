# ADR 0008: A thread may hold at most one write guard per page

Date: 2026-08-28

Status: Accepted

## Context

M10.1 gave `storage::buffer::BufferPool` a real per-frame `RwLock<Page>`
in place of the pin-counter-only `PageGuard` that came before it, so
that a future connection layer could share one pool across threads
(`buffer.MD`). M10.1 was specified as behaviour-preserving — a type-level
`Send + Sync` change with execution still serial underneath it, nothing
a caller should have had to adjust for.

That specification turned out to be wrong in one respect. Before M10.1,
several live `PageGuard`s on the same page, from the same thread, was
ordinary and unremarkable: a guard was just a pin count, and pin counts
stack. `crates/storage/tests/buffer_pool.rs` had two tests built directly
on that assumption — one took a second guard on a page it already held a
guard to in order to prove the first guard's pin outlived a drop of the
other, the other built a vector of four guards on the same page the same
way. Both passed under the pre-M10.1 pin-only `PageGuard`.

After M10.1, `fetch_page`/`new_page` return a `PageWriteGuard` backed by
`frames[idx].page.write()` — a real `std::sync::RwLock` write guard.
`RwLock` is not reentrant: a thread that already holds a write guard on a
page and asks for a second one blocks on itself, forever. Both tests
above self-deadlocked instead of failing, hanging `cargo test --workspace`
rather than reporting anything. `common::sync::recover_lock` is where the
thread parks when this happens; it is not the cause and does nothing
wrong — the deadlock is a direct, correct consequence of asking a
non-reentrant lock to reenter.

## Decision

State the rule M10.1 actually left in place, and enforce it where it is
cheap to check:

- A thread may hold at most one write guard on a given page at a time.
  A second `fetch_page`/`new_page` on that page before the first guard
  drops is a programming error, not a supported pattern — it was never
  actually safe after M10.1 landed, only silently untested.
- Multiple read guards on one page are unrestricted, whether from one
  thread or several: `RwLock::read` is reentrant-safe, which is exactly
  why `fetch_page_read` is the form call sites should reach for whenever
  a page is only being read (`buffer.MD`'s write-guard rule).
- `BufferPool::write_guard`, the sole place a `PageWriteGuard` is minted,
  detects a same-thread double write-guard under
  `#[cfg(debug_assertions)]` via a thread-local `HashSet<FrameId>` of
  frames this thread currently holds a write guard on, panicking with the
  offending page id instead of letting the thread park. This is compiled
  out entirely when `debug_assertions` is off, so it costs a release
  build nothing and adds no failure mode a production run wouldn't have
  had anyway — the detector exists to make the mistake loud in
  development and CI, not to guard against it at runtime.
- The two affected tests were rewritten to establish page content through
  one write guard, drop it, and take every further live guard as
  `fetch_page_read` — keeping the exact property each test proves (a live
  guard keeps its page unevictable, and a pin count only reaches zero
  after every guard drops) while only changing which lock mode exercises
  it, since `unpin_frame` does not distinguish where a pin came from.

## Consequences

Every call site above `storage` that currently interleaves a read and a
write through one `PageWriteGuard` on the same page in one critical
section (`btree::Node`, `heap::SlottedPage`) remains correct and
unaffected — that pattern reuses a single guard rather than acquiring a
second one, which this rule never forbade. Nothing changes for them.

A related, separate finding surfaced while auditing call sites for this
ADR, reported here rather than fixed: `BTreeIndex::insert` (both its
leaf and internal-node branches) and `TableHeap::insert_tuple` each
speculatively acquire a write guard to call `will_fit`/`SlottedPage::insert`
purely to test whether the target page has room, and when it doesn't,
drop that guard having only ever read through it. That page's write latch
was held exclusively for a computation that turned out to be read-only.
Converting those call sites to check fitness under a read guard first is
a behaviour-relevant change — it changes what a concurrent reader can see
interleaved with an insert that is about to split — and is deliberately
left for its own commit and its own crash-injection run rather than
folded into this one.

M10.2's lock manager and any future latch-crabbing implementation must
respect this rule by construction: a descent that needs to hold a parent
latch while acquiring a child's must do so with *two distinct guards on
two distinct pages*, never a second guard on a page it already holds one
for. The debug-build detector exists specifically so the first time a
future code path violates that accidentally, it panics with a clear
message naming the page instead of hanging the test suite the way the
two rewritten tests did here.
