# lock_manager

Grants and releases locks under strict two-phase locking: every lock a
transaction acquires stays held until that transaction ends, at which
point every one of its locks releases atomically via `release_all` - no
partial, mid-transaction unlock exists, since strict 2PL forbids one.
`&self` throughout, not `&mut self`: a single `LockManager` is shared
across every worker thread running a statement against the same database
(`docs/ROADMAP.md`'s M10.2), so it protects its own state with a `Mutex`
and blocks waiters on a `Condvar` rather than requiring a caller to
serialize access itself. Two independent resource spaces share one
instance - row locks, keyed by `Rid`, and table locks, keyed by
`TableId` - so a table-level lock and a row-level lock on a row inside
that table are unrelated as far as this module is concerned; the executor
takes both, in that order (`executor::SeqScanExecutor`,
`executor::IndexScanExecutor`, `executor::InsertExecutor`).

`TransactionManager` owns one `LockManager` per database (`manager.MD`)
and shares it, via `Arc`, with every `executor::ExecutorContext` built for
a statement running against that database: `SeqScanExecutor` and
`IndexScanExecutor` take a table-level `Shared` lock in `init` and a
row-level `Shared` lock per tuple `next` returns, `InsertExecutor` takes a
table-level `Exclusive` lock in `init` and a row-level `Exclusive` lock
per row it writes, and `TransactionManager::commit`/`abort` both call
`release_all` so every lock a transaction acquired releases the moment it
ends.

## Key Components

- `LockMode` - the mode a lock is held in. `Shared` allows multiple
  transactions to hold a lock on the same resource simultaneously; used
  for reads. `Exclusive` allows only one transaction to hold a lock on a
  resource at a time, and it excludes shared locks too; used for writes.
  `LockMode::conflicts_with(self, other) -> bool` - private. `true` unless
  both sides are `Shared`.
- `LockManager` - the lock manager. `state: Mutex<State>` holds every
  resource's current holders and every blocked transaction's pending
  request; `released: Condvar` wakes every blocked call after any
  `release_all`.
- `LockManager::new() -> Self` - creates a lock manager with no locks
  held.
- `LockManager::lock(&self, txn_id, rid, mode) -> Result<(), TxnError>` -
  acquires a lock on `rid` in `mode` on behalf of `txn_id`. Returns
  immediately if `txn_id` already holds `mode` or a stronger mode on
  `rid`; upgrades a held `Shared` to `Exclusive` in place if `txn_id` is
  the only holder; otherwise blocks the calling thread on `released`
  until the conflicting lock(s) are released, unless granting would first
  require blocking behind a cycle in the wait-for graph, in which case it
  returns `TxnError::DeadlockVictim` immediately without blocking at all.
  Returns `TxnError::LockAfterUnlock` if `release_all` has already run for
  `txn_id`.
- `LockManager::lock_table(&self, txn_id, table_id, mode) -> Result<(), TxnError>` -
  the same contract as `lock`, on the table-id resource space instead of
  the row-id one.
- `LockManager::release_all(&self, txn_id)` - releases every lock
  `txn_id` currently holds, across both resource spaces, in one step;
  marks `txn_id` as finished (a further `lock`/`lock_table` call for it
  fails with `LockAfterUnlock` rather than silently reacquiring); and
  wakes every thread blocked in `lock`/`lock_table`, which each recheck
  their own request against the new state rather than assuming they were
  the one released.
- `LockManager::held_lock_count(&self, txn_id) -> usize` -
  `#[cfg(any(test, feature = "test-util"))]`. The number of resources (row
  and table locks combined) `txn_id` currently holds; `0` for a
  transaction holding nothing or already released. Test-only
  introspection, for asserting that `release_all` actually emptied a
  transaction's lock set rather than inferring it indirectly.
- `Resource` - private. `Row(Rid)` or `Table(TableId)`: the two kinds of
  thing a lock can be held on, unified so the wait-for graph walk in
  `would_deadlock` doesn't need to know which kind it's looking at.
- `State` - private. `holders` maps a `Resource` to every `(TxnId,
  LockMode)` currently granted on it (at most one entry per `TxnId`);
  `held_by` is the reverse index `release_all` uses to find everything a
  transaction holds without scanning `holders`; `waiting_for` maps a
  blocked `TxnId` to the single `(Resource, LockMode)` it's parked on - a
  transaction can only ever be blocked on one request at a time, since
  `lock`/`lock_table` itself blocks the calling thread; `finished` is the
  set of transactions `release_all` has already run for.
- `conflicting_holders(state, resource, mode, exclude) -> Vec<TxnId>` -
  private. Every transaction other than `exclude` currently holding
  `resource` in a mode that conflicts with `mode`.
- `would_deadlock(state, start, blockers) -> bool` - private. Walks the
  wait-for graph outward from `blockers` (the transactions `start` is
  about to wait on): for each one, if it is itself blocked on some
  resource, follows the chain to whichever transactions are blocking
  *it*, and so on. Returns `true` the moment `start` itself is reached,
  meaning granting `start`'s wait would close a cycle. Called before
  `start` ever registers itself as a waiter, so the check only ever sees
  waits that are already real, never one it just created.

## Usage Example

```rust
use std::sync::Arc;

use common::{PageId, Rid, TxnId};
use txn::{LockManager, LockMode};

let locks = Arc::new(LockManager::new());
locks.lock(TxnId(1), Rid::new(PageId(0), 0), LockMode::Exclusive)?;
// ... do the write this lock protects ...
locks.release_all(TxnId(1));
```

Every fallible call in this crate returns `TxnError`; see `error.MD` for
how a `LockManager` caller's `DeadlockVictim`/`LockAfterUnlock` fold into
`common::Error` at the point they cross into `engine`.
