# ADR 0004: State the current ACID scope precisely

Date: 2026-08-23

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

## Decision

State precisely what holds, so the claim is credible instead of assumed:

**Atomicity** and **durability** hold, and are real: every write is
WAL-logged before it touches a page (`BufferPool::flush_frame`'s
write-ahead rule), `TransactionManager::commit` force-flushes the log up
to its `Commit` record before returning, and `storage::recovery::recover`
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

**Isolation** holds today, but trivially: `engine::Database` executes one
statement at a time on a single thread, so there is no concurrent
transaction for one transaction's uncommitted writes to be isolated
*from*. `txn::LockManager` and `txn::mvcc` exist as scaffolding but are
not wired into `Database::execute` or the executor - no lock is acquired,
no version chain is consulted. The instant two transactions can run
concurrently (multiple connections, or any async/threaded execution
model), this stops holding by default and isolation must be built for
real: two-phase locking via the existing `LockManager`, or snapshot
isolation via `mvcc::VersionChain`, wired into every read and write path.

## Consequences

Documentation and any future client-facing description of this database
must not claim serializable isolation or constraint-checked consistency -
only what's stated above. The moment concurrent execution is introduced
(the roadmap's eventual M10), this ADR's isolation section is obsolete and
must be revisited alongside whatever locking or MVCC strategy gets wired
in; that revisit is the trigger, not a calendar date. Nothing about
atomicity or durability's scope should need to change then - those are
already complete for a single transaction regardless of how many run
concurrently.
