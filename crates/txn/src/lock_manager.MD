# lock_manager

Grants and releases row-level locks under strict two-phase locking,
blocking (conceptually; the actual wait/wound policy is future work)
conflicting requests and detecting deadlocks among waiters.

## Key Components

- `LockMode` - the mode a lock is held in. `Shared` allows multiple
  transactions to hold a lock on the same resource simultaneously; used
  for reads. `Exclusive` allows only one transaction to hold a lock on a
  resource at a time, and it excludes shared locks too; used for writes.
- `LockManager` - the lock manager. Holds `holders`, a map from `Rid` to
  the list of `(TxnId, LockMode)` pairs currently granted on it.
- `LockManager::new() -> Self` - creates a lock manager with no locks
  held.
- `LockManager::lock(&mut self, txn_id, rid, mode) -> Result<(), TxnError>` -
  acquires a lock on `rid` in `mode` on behalf of `txn_id`, blocking (or
  erroring, once deadlock detection exists) if it conflicts with an
  incompatible lock already held by another transaction. Unimplemented
  (`todo!`).
- `LockManager::unlock(&mut self, txn_id, rid) -> Result<(), TxnError>` -
  releases `txn_id`'s lock on `rid`. Under strict 2PL this only happens at
  commit/abort, never mid-transaction. Unimplemented (`todo!`).

## Usage Example

```rust
use common::{PageId, Rid, TxnId};
use txn::{LockManager, LockMode};

let mut locks = LockManager::new();
locks.lock(TxnId(1), Rid::new(PageId(0), 0), LockMode::Exclusive)?;
locks.unlock(TxnId(1), Rid::new(PageId(0), 0))?;
```

Not yet wired into `TransactionManager` or `engine::Database`: this crate
provides the type and its intended contract ahead of the executor
actually taking locks around row reads and writes.
