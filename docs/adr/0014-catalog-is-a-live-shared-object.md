# ADR 0014: The catalog is a live shared object, and no lock on it spans a statement

Date: 2026-09-07

Status: Accepted

## Context

**This section describes the tree as it stood at commit `60e37e2`, before
this decision was implemented**; its citations are historical, and the
Decision below is what the code should do.

The engine held its catalog as `EngineShared.catalog: RwLock<Catalog>` and
used that one lock for three different jobs at three different durations:

- **A statement's whole lifetime.** `EngineShared::run` took the *read*
  lock before building the executor and held it across `init` and the
  entire `next` loop, because `ExecutorContext` borrows a `&Catalog`. A
  `SELECT` over a large table therefore held it for seconds.
- **A catalog mutation.** `CREATE TABLE`/`CREATE INDEX` took the *write*
  lock; an index root split took no lock at all, mutating through the
  shared `&Catalog` via an `AtomicU32` inside `IndexInfo`
  (`IndexInfo::set_root_page_id`, called from `Catalog::update_index_root_page`,
  itself `&self`) while its caller held only the read lock.
- **A reload.** After an abort, `EngineShared::reload_catalog` re-read the
  catalog from disk and replaced the object wholesale.

Two problems came out of that, and they pull in opposite directions.

**P-46: a reload could regress in-memory state.** With `Catalog::open`
running outside the write lock, a reload could read an index's catalog row
before a concurrent insert's root split wrote it and install itself after,
reverting the in-memory root page id to a page the real root's separators
no longer cover, silently stranding later inserts.
`crates/engine/tests/catalog_reload_race.rs` is the regression test. The
fix taken was to widen the write lock to cover `Catalog::open` — i.e. to
exclude *readers* as well as mutators, because a mutation (the root split)
travels under the read lock.

**P-53: that widened lock made one slow statement stall every session.**
The write lock cannot be acquired while a statement holds the read lock, so
a reload — which runs on a worker after a disconnect or an aborted
autocommit statement — parks as a pending writer for the length of that
statement. `std::sync::RwLock` does not let a new reader overtake a queued
writer, so *every* subsequent catalog read waits for the slow statement,
including `TableNames`, which the dispatch thread serves inline.
`crates/engine/tests/dispatch_never_blocks.rs`'s
`a_paused_select_does_not_block_an_unrelated_sessions_request` fails on
exactly this.

The two tests encode requirements that the single-`RwLock` design cannot
satisfy at once, and no reordering of the existing lock fixes it: as long
as a statement holds a lock on catalog state for its duration and a reload
takes the exclusive side of that same lock, an unrelated request either
waits for the statement or races the reload. It is also worth noting that
`dispatch_never_blocks` was passing before the P-46 fix only by accident —
the reload worker happened to block inside `Catalog::open` (a two-frame
buffer pool, `ADR 0010`) before ever reaching the write lock. Had that open
been fast, the pre-fix code would have stalled the same probe.

The question underneath both is one question: **what does a caller hold
while it uses the catalog?** Answering it decides the reload mechanism, the
executor's access path, and what a future catalog mutator (M14's
`DELETE`/`UPDATE`, M15/M16's constraints, M26's `DROP TABLE`) is allowed to
assume.

## Decision

**There is exactly one live `Catalog` object for the lifetime of a
`Database`, it is never replaced, and no lock on it is ever held across
statement execution or across disk I/O other than a reload's own.**

Specifically:

- **The catalog synchronizes itself.** The `RwLock` moves off
  `EngineShared.catalog` and into the `catalog` crate, which owns the type
  and is therefore the only place the policy can be stated once.
  `EngineShared` holds an `Arc<Catalog>`; `Catalog`'s mutators
  (`create_table`, `create_index`, `update_index_root_page`, later
  `drop_table`) take `&self`, not `&mut self`. Callers — the binder, the
  optimizer, `explain`, every executor — hold a `&Catalog` and hold no
  lock; each call takes an internal lock and releases it before returning.
- **Lookups return owned values, not references.** `get_table`,
  `get_table_by_id`, `index_for_column` and `indexes_for_table` return
  `TableInfo`/`IndexInfo` by value (or a small owned projection) instead of
  `&TableInfo`, because a reference into a map behind an internal lock
  would either leak a guard into the caller or reintroduce the
  statement-length hold this ADR exists to remove. These are per-statement
  or per-operator calls, not per-row ones, so the clone is not on a hot
  path.
- **A reload applies into the live object; it does not swap it.**
  `reload_catalog` becomes a `Catalog` method that reads a fresh state from
  disk and installs it into the existing object's maps. Nothing anywhere
  holds a stale `Catalog` that a reload has superseded, because there is
  only ever one.
- **Reloads and mutations exclude each other through a dedicated mutation
  lock, and readers are excluded by neither.** A single `Mutex` inside
  `Catalog` is taken by every mutator for that mutator's duration, and by a
  reload across the whole of its read-from-disk plus install. That is
  precisely the exclusion P-46 needs — a mutation cannot interleave with a
  reload's read — and it is the *only* thing a reload excludes. A `SELECT`
  running for a minute blocks nothing.
- **Lock ordering: the catalog's mutation lock is taken before any buffer
  pool latch, never after.** A mutator holds it across heap writes and a
  reload holds it across `Catalog::open`'s reads, so it sits above the
  buffer pool in the ordering that `docs/agent-guide.md`'s "Latch ordering" invariant
  governs. No code may take it while holding a page latch or a page guard.
- **A reload may block for an unbounded time, and that is allowed, because
  it only ever runs on a worker thread.** It waits on the mutation lock and
  then on the buffer pool (`docs/adr/0010-buffer-pool-waits-for-a-frame.md`).
  Nothing on the dispatch thread may take the mutation lock.

The alternative considered seriously was a **copy-on-write snapshot**:
`RwLock<Arc<Catalog>>`, where a statement clones the `Arc` under a
momentary read lock and runs against that snapshot while a reload publishes
a fresh `Arc`. It is cheaper — `Catalog`'s API would not change — and it
also fixes P-53. It was rejected for two reasons. First, it makes
correctness depend on a rule every future mutator has to remember: a
mutation applied to a superseded snapshot updates the on-disk row but is
lost from the published catalog, which is P-46's corruption reintroduced
through a different door, so `update_index_root_page` and every mutator
added later would have to route to the live handle rather than to the
`&Catalog` the caller already has. Second, it is sound today only because a
writer holds an exclusive table lock, so two writers never split the same
index root concurrently; that property is one this project intends to
weaken (finer-grained locking, MVCC writes), and a design that silently
depends on it is a trap set for the milestone that changes it. The
live-object decision above depends on neither.

## Consequences

`Catalog`'s public API changes from reference-returning to value-returning,
which ripples into `planner` (`binder.rs`, `optimizer.rs`, `explain.rs`,
`logical_plan.rs`, `physical_plan.rs`) and `executor`
(`operators/seq_scan.rs`, `operators/index_scan.rs`, `operators/insert.rs`,
`context.rs`) wherever a getter's result is bound as a reference. The call
sites are few — five in `executor`, a handful in `planner` — but they are a
public-API change and they must land in one commit with the catalog change,
along with every affected sibling `.MD` and `crates/catalog/README.md`.

`EngineShared::run` and `execute_non_control_statement` stop taking a
catalog lock, so `EngineShared.catalog`'s `recover_lock` sites disappear.
The catalog is one fewer thing that can be poisoned by a panicking worker,
since the poison now lives inside `Catalog` and is recovered there.

Both existing regression suites must pass unchanged:
`crates/engine/tests/catalog_reload_race.rs` (P-46's guarantee, now given
by the mutation lock instead of by a write lock over readers) and
`crates/engine/tests/dispatch_never_blocks.rs` (P-53's, now given by the
absence of any statement-length lock). The second passes under this
decision whether or not `TableNames` is served on the dispatch thread,
which makes it a real assertion about the catalog rather than about which
thread answered.

What this does **not** do is make catalog reads transactional. A statement
still sees whatever the live catalog holds at the moment of each call, so
two calls within one statement may observe different states — the same
weakness the previous design had between statements, now visible within
one. That is acceptable while DDL is `CREATE TABLE`/`CREATE INDEX` under an
exclusive table lock, and it is the thing to revisit if DDL ever becomes
concurrent with DML on the same table; `docs/adr/0004-acid-scope.md` is
where that limit belongs when it is stated to users.

This decision touches no storage format, no WAL record and no recovery
path, so it carries no crash-injection obligation of its own — but it does
change code that runs concurrently with the buffer pool, so the existing
sweeps stay the gate they always were.

**Implemented.** The catalog now holds a private `RwLock<CatalogState>`
and a mutation `Mutex`, every method takes `&self`, `Catalog::reload`
replaced the wholesale swap, and `EngineShared` holds an `Arc<Catalog>`
with no lock of its own; the nine `recover_lock(self.catalog…)` sites are
gone. `crates/catalog/src/catalog.MD` documents the resulting discipline,
and `crates/engine/tests/dispatch_never_blocks.MD` and
`crates/engine/tests/catalog_reload_race.MD` each name the other as the
requirement its own fix must not break, since those two suites are what
hold the two halves of this decision apart.
