# ADR 0004: State the current ACID scope precisely

Date: 2026-08-23

Revised: 2026-09-07 — isolation, after M10

Status: Accepted

## Context

M8 adds user-visible `BEGIN`/`COMMIT`/`ROLLBACK` on top of the ARIES
recovery M7 built. The moment "transaction" becomes something a user types
rather than an internal bookkeeping detail, it starts implying the full
ACID contract by name alone — and three of those four letters currently
hold for reasons that have nothing to do with anything built to guarantee
them. Claiming "transactions" without saying which guarantees actually
hold, and why, would be a claim the codebase can't back up: there are no
constraints to enforce consistency beyond type checking, and there is no
lock manager, even though `txn::LockManager` exists as a type. Isolation
holding today is an accident of the engine being single-threaded, not a
property anything enforces.

**Revised after M10.** The isolation section below is the second one this
ADR has had. The first said isolation held trivially, because
`engine::Database` ran one statement at a time on one thread, and it named
the moment concurrent execution arrived as its own revisit trigger. M10.2
shipped that concurrency and M10.3 shipped MVCC, so the trigger fired and
the section was rewritten against what the code now does. Nothing about
atomicity, durability or consistency changed, exactly as the original
predicted.

## Decision

State precisely what holds, so the claim is credible instead of assumed:

**Atomicity** and **durability** hold, and are real: every write is
WAL-logged before it touches a page (`BufferPool::flush_pages`
(`crates/storage/src/buffer.rs:621`) forces the log durable to the
batch's highest `page_lsn` before any page reaches its real location),
`TransactionManager::commit` force-flushes the log up
to its `Commit` record before returning
(`crates/txn/src/manager.rs:79-82`), and `storage::recovery::recover`
runs Analysis/Redo/Undo on every open, undoing anything that never
committed via the same `undo_transaction` a user's own `ROLLBACK` calls.
A transaction's writes are all-or-nothing and, once committed, survive a
crash. This is the ACID content M7 and M8 together actually built.

**Consistency**, here, means only "every write satisfies the schema's
declared column types" — the binder's type-checking
(`planner::Binder::bind_insert`'s coercion and mismatch checks) is the
entire enforcement mechanism. There is no `CHECK`, `UNIQUE`, `NOT NULL`
enforcement beyond nullability bookkeeping, foreign key, or other
constraint machinery. A transaction that type-checks can still leave the
database in a state a real schema would have rejected.

**Isolation** is **snapshot reads over two-phase-locked writes**, and it
is enforced rather than accidental. Statements from different sessions run
concurrently on a fixed worker pool
(`engine::runtime::WORKER_POOL_SIZE = 8`,
`crates/engine/src/runtime.rs:34,179`); one session's own statements stay
serialized behind its session mutex.

*Readers take no locks.* Every user statement runs at
`IsolationLevel::SnapshotIsolation` (`crates/engine/src/runtime.rs:773,852,883`),
and under it `SeqScanExecutor::init` skips the shared table lock
(`crates/executor/src/operators/seq_scan.rs:26-29`) while `next` filters
each tuple through `VersionStore::is_visible_to(rid, txn_id, read_ts)`
(line 43). `IndexScanExecutor` does the same
(`crates/executor/src/operators/index_scan.rs:45,64`). A reader therefore
never blocks and never blocks anyone.

*Writers take exclusive locks and hold them to commit.*
`InsertExecutor::init` takes an exclusive **table** lock
(`crates/executor/src/operators/insert.rs:35`) and `next` takes an
exclusive lock per inserted `Rid` (line 72). Nothing releases a lock
early: `TransactionManager::commit` and `abort` call
`LockManager::release_all` as their last step
(`crates/txn/src/manager.rs:88,103`). That is strict two-phase locking, so
two writers against one table serialize; a waiter that would close a
cycle in the wait-for graph is aborted as a deadlock victim
(`crates/txn/src/lock_manager.rs:52-67,168-169`), and its wait is bounded
per `docs/adr/0012-bounded-lock-waits.md`.

*The snapshot is per-process and in memory.* `read_ts` is assigned at
`begin` and `commit_ts` at commit, both from one counter that starts at 0
each time the process starts (`crates/txn/src/manager.rs:22,51-54,72,83`).
`VersionStore` is a `Mutex<StoreState>` whose `chains` field is a
`HashMap<Rid, VersionChain>` (`crates/txn/src/version_store.rs:10-19`)
that no restart survives, and **a
`Rid` with no chain is visible to everyone** (lines 89 and 97). That
default is what makes an empty store after recovery correct: ARIES has
already undone every loser physically, so what remains in the heap is
committed, and no snapshot from before the crash exists to be confused.

*What this is not.* It is not serializable: SI permits write skew in
general. It is not `SET TRANSACTION ISOLATION LEVEL` — no grammar
produces one, and `IsolationLevel`'s five variants
(`crates/txn/src/isolation.rs`) collapse to exactly two behaviours in the
executors, snapshot isolation and "take shared locks". Only two are
reachable at all: `SnapshotIsolation` for every user statement, and
`ReadCommitted` for the checkpointer's own transaction
(`crates/txn/src/checkpoint.rs:21`). `ReadUncommitted`, `RepeatableRead`
and `Serializable` are names with no behaviour behind them.

*There is no first-committer-wins check, and none is needed yet* — but
not for the reason it might look. It is not that `INSERT` is the only
write; it is that a writer holds an **exclusive table lock** for its whole
transaction, so two writers are never concurrent against the same table in
the first place. The day a write path takes anything weaker than that lock
— which is the day `UPDATE` and `DELETE` arrive with row-level conflicts —
SI needs a write-write conflict rule of its own, and this section needs
its third revision.

## Consequences

Documentation and any future client-facing description of this database
must not claim serializable isolation or constraint-checked consistency -
only what's stated above. In particular "MVCC" here means snapshot reads,
not snapshot writes: the write path is locking, and the concurrency a user
actually observes is many concurrent readers against one writer per table.

Atomicity and durability were unaffected by M10, exactly as this ADR's
first version predicted: they are complete for a single transaction
regardless of how many run concurrently.

Two later decisions now depend on this section and are recorded
separately rather than here.
`docs/adr/0012-bounded-lock-waits.md` fixes what a writer's lock wait
costs. `docs/adr/0013-version-identity-and-lifetime.md` fixes what
identifies a version and when its chain may be discarded, which is what
makes the in-memory store above bounded — M10.4 in `docs/ROADMAP.md`.

**The next revisit trigger is M14.** `UPDATE` and `DELETE` introduce the
first writes that supersede a version, and with them every question this
section currently answers by "the exclusive table lock makes it moot":
write-write conflicts under SI, whether a deleted row stays visible to an
older snapshot, and whether the write path can drop to row-level locking.
When M14 lands, this section is revised again — that milestone is the
trigger, not a calendar date.
