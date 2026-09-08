# ADR 0013: A version chain is keyed by a durable row id and pruned against the oldest active snapshot

Date: 2026-09-07

Status: Accepted

## Context

**This section describes `crates/txn` as it stood after M10.3, which is
the state this decision was taken against.** M10.4 has since implemented
the Decision below, so the shapes described here are the ones it
replaced: the store is now a `Mutex<StoreState>` carrying `chains`,
`write_sets` and `prune_candidates`
(`crates/txn/src/version_store.rs:10-19`). Line numbers are therefore
omitted here where the code has moved — a historical account is not a
map of the current tree, and a citation that resolves to something else
is worse than none.

M10.3 shipped MVCC visibility: `txn::VersionStore` kept a
`Mutex<HashMap<Rid, VersionChain>>`,
`crates/executor/src/operators/insert.rs` records a version per inserted
row, and the scan executors ask the store whether a `Rid` is visible to
the reader (`crates/executor/src/operators/seq_scan.rs`). It works, and it is
sound today for one reason that is about to stop being true: a `Rid` can
currently have at most one version, and no row is ever removed.

**Identity.** A `Rid` is a `(PageId, slot)` pair. It identifies a *slot*,
not a *row*, and it is unique only while that slot stays occupied. M14
introduces `DELETE` and in-page compaction — its roadmap entry commits to
"keeping slot indices stable since a `Rid` is half slot index", which
holds for live tuples but says nothing about a tombstoned slot, which is
exactly what a later `INSERT` reuses. The moment it does,
`VersionChain::push` (`crates/txn/src/mvcc.rs:20-22`) prepends the new
row's version to the deleted row's chain, and `visible_version_for`
(lines 28-32) walks one list holding versions of two unrelated rows. A
snapshot older than the delete then sees the wrong row, or the new row is
hidden from a reader entitled to it, depending on the timestamps. Nothing
detects this: it is a silent wrong answer, and it becomes reachable the
day `DELETE` lands.

**Lifetime.** The store was only ever appended to. `record_insert` added
a chain per inserted `Rid`; `commit_versions` stamped `begin_ts`;
`abort_versions` removed the aborting transaction's entries but kept the
emptied chain; there was no other mutator. Two costs followed. The map
grew monotonically with every row
inserted since process start, with no ceiling and no knob — the exact
opposite of the "bounded memory over an unbounded database" property M2
exists to give and the buffer pool enforces for pages. And commit and
abort both iterated **every chain in the map** under the single store
mutex, so commit latency scaled with the whole database rather than with
the transaction's write set, while readers contended on that same mutex
once per tuple.

The two are one question — what identifies a version, and how long does
it live — because both are answered by the same missing thing: a name for
a row that outlives its slot, and a rule for when a chain stops mattering
to anybody. Deciding them apart risks deciding them inconsistently, in
the milestone that can least afford it.

One property of the code shapes everything below, and it still holds:
`is_visible` and `is_visible_to` return **`true` when the key is absent**
(`version_store.rs:89,97`). "No chain" means "visible to everyone".
That default is what makes pruning possible at all — a chain nobody needs
can simply be dropped — and it is also the trap: dropping the chain of a
*deleted* row resurrects it.

## Decision

### Identity: a durable row id, allocated once, never reused

Version chains are keyed by a `RowId(u64)` — a monotonically increasing
identifier assigned when a row is first inserted, stamped into the heap
tuple header, and carried forward unchanged when `UPDATE` rewrites the
row as a delete-plus-insert. A row keeps one id for its whole life,
across page moves and slot changes; an id is never reused, including
across a restart. `Rid` stays what it is — the physical address the heap
and the index use to find a tuple — and stops being an identity.

M14 is where this lands, because M14's entry already commits to reserving
space in the heap tuple header for version metadata before the on-disk
format is in use. **That reserved space is this id**, and this ADR is what
that instruction was reserving space for.

- **`RowId(0)` is reserved** and means "unstamped". A tuple whose id
  reads zero was written before this field existed, which keeps the
  all-zero-page invariant in `CLAUDE.md` intact: an untouched page still
  decodes as a valid, empty page.
- **Allocation is from a durable high-water mark**, a `next_row_id: u64`
  field in the page-0 file header beside `header::VERSION_RANGE`
  (`crates/storage/src/disk.rs:19`), which is versioned and mutated
  through `PageWriteGuard::write` like any other page. Ids are handed out
  from an in-memory block claimed by bumping the persisted mark
  (block size an implementation choice; 1024 is a reasonable start), so
  the common insert does not write page 0. A crash therefore loses the
  unused tail of the current block: ids have gaps and are monotonic, and
  nothing may depend on them being dense. The same block-allocation
  pattern is what M17's sequences and `SERIAL` need, so this is machinery
  the project buys once.
- **Durability of the counter is not optional.** The version store itself
  is in-memory and per-process, so an empty store after a restart is
  correct — no snapshot survives a restart to be confused. But the id is
  stamped *on disk*, in tuples that do survive, so a counter restarting at
  1 would hand a fresh row the id of an existing one and reproduce the
  collision this decision exists to remove, across restarts instead of
  across slot reuse.

Rejected alternatives. **Keep the `Rid` key and erase the chain when the
slot is freed** requires slot reuse and chain removal to be one step as
seen by every concurrent reader; that is a lock ordering problem between
the heap and the version store, in the middle of the compaction path, and
it buys only the saving of one header field. **Never reuse a slot**
trades away the space reclamation M14 exists to deliver. **Derive the id
from the insert's LSN** is attractive — the WAL already produces durable,
monotonic, never-reused numbers — but the id has to be in the tuple's
bytes while those bytes are what the log record describes, so the record
would have to mean "row id = my own LSN" and redo would re-derive it. It
saves a header field at the cost of coupling row identity to the log
format and constraining recovery; not worth it.

### Lifetime: write-set-scoped bookkeeping, pruned against a watermark

**Commit and abort touch only their own chains.** The store itself tracks,
per writing `TxnId`, the set of rows that transaction has written, under
the same mutex as the chains, and `commit_versions`/`abort_versions` visit
that set instead of iterating the map. Commit cost then scales with the
transaction's write set, which is the only thing it should ever have
scaled with, and a large database stops making every commit slower for
everyone.

The write set belongs to the store and **not** to `txn::Transaction`,
beside the `read_ts` and `begin_lsn` it already carries, even though that
is the more natural home: `EngineShared::run`
(`crates/engine/src/runtime.rs:980-987`) hands the executors a *clone* of
the `Transaction`, so anything `InsertExecutor`
(`crates/executor/src/operators/insert.rs:69-71`) recorded on
`ExecutorContext::txn` (`crates/executor/src/context.rs:8`) would be
dropped with that clone and the manager's copy would see an empty write
set for every real statement.

**The prune rule is one watermark.** `TransactionManager` exposes the
oldest `read_ts` among active transactions — the exact analogue of the
`earliest_active_begin_lsn` it already computes
(`crates/txn/src/manager.rs:121`) — and when there is no active
transaction the watermark is the next timestamp to be issued, which
prunes everything prunable. Against that watermark:

- A chain whose newest version is **committed with `begin_ts <=`
  watermark and has `end_ts == None`** may be dropped. Every present and
  future reader sees that version, and "no chain" already means exactly
  that, so dropping it is invisible.
- A chain whose newest version has **`end_ts` set and `end_ts <=`
  watermark** describes a row no snapshot can still see. It may be
  dropped **only together with the heap tuple it describes** — because
  "no chain" means visible, so a chain dropped while a tombstoned tuple
  is still readable resurrects a deleted row.
- A chain **emptied by an abort** stays in the map at abort time and is
  dropped later, against the same watermark. `abort_versions` may not
  remove it on the spot, however empty it is: a scan copies a tuple's
  bytes out from under the page latch and only *then* asks `is_visible_to`
  (`crates/executor/src/operators/seq_scan.rs:38-51`; the index scan
  checks visibility *before* reading the heap, `index_scan.rs:66-85`, so
  there the same window surfaces as a `CorruptTuple` error over a row the
  undo has already removed), while `TransactionManager::abort` undoes the
  pages first and empties the chains after
  (`crates/txn/src/manager.rs:94-106`), so a reader can be holding the
  bytes of a row whose undo has already run. The empty chain is what makes
  that reader drop the row; "no chain" would make it yield a rolled-back
  one. The chain becomes prunable once the watermark reaches the timestamp
  the store stamps on it at abort — the next timestamp to be issued at
  that moment — because every transaction that could still be holding
  those bytes was active then, therefore has a `read_ts` below that stamp,
  and the watermark cannot reach it until all of them have finished.
- Everything else stays.

**That second clause is this ADR's constraint on M14, and the reason the
two entries were one decision.** In-page compaction and version pruning
are gated by the same number: a tombstoned slot may be physically
reclaimed once the deleting transaction's commit timestamp is at or below
the watermark, and that is the same instant its chain becomes prunable.
M14 must not compact a tombstone on the grounds that the delete
committed — it has to be a delete no live snapshot predates — and it gets
that for free by asking the same watermark the pruner asks.

Pruning runs where the watermark changes — on commit and on abort, when
a transaction leaves the active set — rather than on a timer or a
background thread. There is no vacuum process in this design and this ADR
does not add one.

## Consequences

`VersionStore`'s key type changes from `Rid` to `RowId`, so every caller
that looks a row up by `Rid` today
(`crates/executor/src/operators/seq_scan.rs:43`,
`crates/executor/src/operators/index_scan.rs`) must carry the row id out
of the heap tuple it just read instead. That is a wider change than it
looks: it is the reason the tuple header field has to exist before the
key changes, and therefore the reason this is M14-shaped work rather than
a patch to `crates/txn`.

The heap's on-disk tuple format gains a field, so `HEADER_VERSION`
(`crates/storage/src/disk.rs`) is bumped and `VERSION_RANGE`'s accepted
range moves with it. Any database file written before that bump is not
readable afterwards; this project has no released format to be
compatible with, and the versioned header exists precisely so the failure
is a clean rejection at open rather than a misparse.

Commit stops being O(rows inserted since startup) and becomes O(write
set). Once that is true, the store's single `Mutex` is worth revisiting —
an `RwLock` or a sharded map would let concurrent readers stop serializing
on `is_visible_to` — but this ADR does not require it, because with the
whole-map scan gone the mutex is held for a few entries at a time rather
than for the length of the database.

Memory becomes bounded by *live, contended* rows rather than by rows ever
inserted: a database with no long-running transaction prunes to almost
nothing, and a database with one ancient snapshot open pins exactly the
chains that snapshot might still need. That is the standard MVCC
trade-off and it is now an explicit one — a transaction left open holds
version state proportional to what has changed since it started, which
`idle_in_transaction_timeout_ms` already bounds in time.

`VersionStore::is_visible` (`version_store.rs:85`) has no caller outside
tests; the live path is `is_visible_to` (line 93). Whichever milestone
implements this should delete it rather than port it, unless the ADR 0004
revisit finds a reason for a reader with no transaction id.

The work is a sub-milestone rather than an entry in the problem queue:
`docs/ROADMAP.md`'s **M10.4 — Bounded, prunable version storage** carries
the write-set tracking, the watermark and the prune rule for live rows,
and M14 extends the same watermark to tombstones and compaction. The
prevention that must ship with M14, stated here so it cannot be lost: a
test in `crates/executor/tests/` that deletes a row, inserts another into
the freed slot in a later transaction, and asserts a snapshot opened
before the delete still sees the old row and never the new one — the
assertion that fails loudly if the two rows ever share a chain again.
