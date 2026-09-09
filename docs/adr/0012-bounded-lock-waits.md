# ADR 0012: Lock waits are bounded, and `55P03 lock_not_available` is part of this engine's contract

Date: 2026-09-06

Status: Accepted

## Context

M10.2 replaced the engine's single execution thread with a fixed worker
pool and deleted the park queue that used to serialize explicit
transactions. Two things survived that deletion without a mechanism
behind them, and one liveness hazard arrived with it.

**This section describes the tree as it stood before this decision was
implemented during M10** (commit `3da0d74`); its citations are historical,
and the Decision below is what the code does now.

**The knob and the error outlived their implementation.**
`common::DbConfig::lock_wait_timeout_ms` (default 5000 via
`DEFAULT_LOCK_WAIT_TIMEOUT_MS`) was read by nothing outside its own
construction. `common::Error::LockTimeout` mapped to
`SqlState::LOCK_NOT_AVAILABLE` but no code path constructed it:
`txn::LockManager::acquire` waited on a `Condvar` with no deadline at all
— `state = recover_lock(self.released.wait(state), …)` — and its only
escape was `would_deadlock`, which finds cycles in the wait-for graph and
nothing else. Two sibling `.MD` files still described both as live
behaviour tied to the deleted park queue, so the documentation promised a
`55P03` the engine could not raise.

**An untimed wait plus a fixed pool is a wedge.**
`crates/engine/src/runtime.rs` fixed `WORKER_POOL_SIZE = 8`, and every
statement ran on that pool (`crates/engine/src/worker_pool.rs`); requests
queued behind it in an
`mpsc::sync_channel`. Eight sessions blocked on a table lock held by a
ninth occupy all eight workers. The ninth session's `COMMIT` — the one
statement that would release the lock — is queued behind them and can
never be dispatched. No cycle exists in the wait-for graph, so nothing is
aborted, and the engine recovers only when `expire_idle_transaction`
(`runtime.rs`, called from the engine thread's 50 ms tick) reaches the
holder after `idle_in_transaction_timeout_ms` (60 s by default) — after
which the queued `COMMIT` finally runs, against a transaction that was
aborted underneath it. Today this needs nine in-process
`Database::connect` handles, so it is reachable but unlikely. M13.2 puts
a `pgwire` listener in front of the same pool, where nine concurrent
clients are the ordinary case rather than a contrived one.

The question underneath all three is one question, which is why it is
answered once here: **does a lock wait in this engine have a bound?**
Answering it decides whether the knob and the error variant are deleted
or made live, and it decides what the pool-exhaustion fix is allowed to
assume.

The alternative considered seriously was **admission control** — never
letting the last free worker be occupied by a statement that is about to
block. It was rejected for three reasons. It cannot be applied where the
decision is made: a lock is taken deep inside an executor
(`crates/executor/src/operators/insert.rs`), long after the
statement has been dispatched to a worker, so the engine cannot know in
advance that a statement will block. It fixes the wrong half of the
problem: even with a worker permanently reserved, a session that holds a
lock and then stops issuing statements — a client that walked away, which
over a network is routine — blocks every waiter forever, because nothing
bounds the wait itself. And it would be the second, contradictory answer
to a question this repository has already answered once:
`docs/adr/0010-buffer-pool-waits-for-a-frame.md` decided that a thread
contending for a scarce resource waits, with a deadline, and reports a
distinct error when the deadline expires. A lock is the same shape of
resource as a frame.

## Decision

**A lock wait is bounded.** `txn::LockManager::acquire` waits with a
deadline instead of waiting indefinitely, and `55P03 lock_not_available`
is part of this engine's contract to a client. `lock_wait_timeout_ms` and
`Error::LockTimeout` are kept and made live rather than deleted.

Specifically:

- **The bound is per lock acquisition**, not per statement and not per
  transaction — the same unit Postgres's `lock_timeout` uses. A statement
  that takes three locks may therefore wait up to three times the
  configured timeout in the worst case; that is accepted, because the
  alternative (a statement-wide budget) requires threading a deadline
  through every executor and buys nothing this decision needs.
- **The deadline is absolute, computed once on entry** to `acquire`, and
  re-checked around the retry loop. `Condvar::wait_timeout` returns on
  every notification and on spurious wakeups, and `acquire`'s loop
  re-evaluates conflicts on each wake; a timeout re-derived from
  `Instant::now()` inside the loop would restart the clock on every
  unrelated `notify_all` and would not be a bound at all. This is the one
  implementation detail that decides whether the ADR is honoured or
  silently defeated.
- **Expiry raises `Error::LockTimeout`** through a new `TxnError`
  variant, alongside `DeadlockVictim` in `crates/txn/src/error.rs`, so
  the mapping to `SqlState::LOCK_NOT_AVAILABLE` already at
  `crates/common/src/error.rs:170` is the one a client sees.
- **A lock timeout has the same effect on transaction state as a
  deadlock abort.** Both mean "the lock manager refused"; a client must
  not have to distinguish two failure shapes for one condition, and the
  engine must not grow a second recovery path.
- **`Error::LockTimeout` is retryable.** `Error::is_retryable`
  (`crates/common/src/error.rs:218-225`) currently matches
  `SERIALIZATION_FAILURE`, `DEADLOCK_DETECTED` and
  `STATEMENT_COMPLETION_UNKNOWN`; `LOCK_NOT_AVAILABLE` joins them,
  because a lock held by another transaction is by definition transient.
  Without this the bounded wait converts a hang into an error a client is
  told not to retry, which is a worse contract than the hang.
- **`lock_wait_timeout_ms = 0` means wait forever**, matching Postgres's
  `lock_timeout = 0`, as the escape hatch for a batch job that would
  rather block than fail. The default stays 5000 ms — deliberately finite
  where Postgres's default is infinite, because this engine has a fixed
  eight-worker pool, no DBA watching it and no external way to cancel a
  statement, so an unbounded default lets one stuck session end the
  process's usefulness.

**The pool-exhaustion hazard is closed by this bound rather than by a
separate mechanism.** Eight blocked waiters now fail after at most
`lock_wait_timeout_ms`, free their workers, and let the holder's `COMMIT`
run: the engine self-heals in 5 s by default instead of 60 s, and it heals
by erroring statements a client is told to retry rather than by aborting
a transaction underneath a queued `COMMIT`. What this does **not** do is
raise the ceiling: eight concurrent blocked statements still occupy the
whole pool for the duration of the timeout, and that remains true until
some later milestone stops a blocked statement from holding a worker at
all. That is a throughput limit with a bounded, documented worst case,
which is a different class of problem from a wedge with no exit.

## Consequences

`DbConfig::lock_wait_timeout_ms` becomes a knob that does something, and
`Error::LockTimeout` becomes an error a client can actually receive.
Anything that treated `LockManager::acquire` as infallible-except-deadlock
must now handle a second refusal, and `crates/common/src/config.MD` and
`crates/common/src/error.MD` must stop describing the deleted park queue
and describe this instead.

`crates/engine/tests/sessions.rs:64`'s
`a_second_session_can_begin_immediately_without_55p03` stays correct and
should stay: `BEGIN` takes no locks, so a second session's `BEGIN` must
still never report `55P03`. What this ADR makes raisable is a `55P03`
from a *conflicting statement*, not from starting a transaction — the
distinction that assertion exists to protect.

The engine's worst-case recovery from lock contention becomes
`lock_wait_timeout_ms` (5 s) rather than `idle_in_transaction_timeout_ms`
(60 s), and the two knobs now have a relationship worth stating: the idle
timeout is the backstop for a session that holds locks and stops talking;
the lock timeout is the bound for everyone waiting on it. The idle
timeout should stay the larger of the two, or waiters will outlive the
holder's expiry and the bound will never be the thing that fires.

M13.2 inherits a bounded failure instead of a hang: a network client that
takes a lock and goes quiet costs every concurrent statement at most one
timeout each, and the listener sees `55P03` with `is_retryable() == true`
rather than a pool that stops answering. Its roadmap entry records this,
including the remaining ceiling — that a blocked statement still occupies
a worker for the length of its wait.

This decision constrains, and is constrained by, nothing else in flight
except the lock manager itself; it does not touch storage, the WAL,
recovery or the buffer pool, so it carries no crash-injection
obligation. The implementation is `crates/txn`, `crates/common` and their
tests, filed as its own problem entry rather than done here, and the two
tests that must ship with it are named there: one that a lock wait past
`lock_wait_timeout_ms` really returns `LockTimeout`, and one that
`WORKER_POOL_SIZE + 1` sessions, all but one blocked on a lock the last
one holds, still lets the holder's `COMMIT` complete within a bounded
timeout.
