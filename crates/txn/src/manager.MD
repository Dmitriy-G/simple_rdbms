# manager

Owns the lifecycle of every transaction: assigning ids, tracking active
transactions, and driving commit/abort — which means writing the
corresponding log record(s) and, for abort, undoing whatever the
transaction had already written.

## Key Components

- `TransactionManager` - the manager. Holds `active`, a map from `TxnId`
  to `Transaction` for every currently in-flight transaction, and
  `next_txn_id`, the counter handing out fresh ids.
- `TransactionManager::new(highest_seen) -> Self` - creates a manager with
  no active transactions, handing out ids starting past `highest_seen` —
  the highest `TxnId` `recovery::recover` observed anywhere in the log, or
  `None` for a log that has never recorded a real transaction. Seeding
  from this rather than always starting at `0` is what stops a freshly
  opened database from ever reusing an id the log has already used: a
  reused id's new `Begin` would otherwise chain its `prev_lsn` onto an
  unrelated transaction's own log records, and — if that old id was a
  "phantom winner" left by a crash between a `Commit` and its `End` —
  could even make a brand new, never-committed transaction look already
  committed.
- `TransactionManager::begin(&mut self, pool, isolation_level) -> Result<TxnId, TxnError>` -
  begins a new transaction at the given isolation level: assigns it the
  next id, logs a `Begin` record, and tracks it as active. Returns its id.
  A debug assertion checks the freshly assigned id exceeds every already-
  active id, catching an id counter that was not seeded past every id the
  log has ever used.
- `TransactionManager::commit(&mut self, txn_id, pool) -> Result<(), TxnError>` -
  commits `txn_id`: logs a `Commit` record and force-flushes the log up to
  it (so an acknowledged commit can never be lost to a crash), logs `End`,
  and removes it from the active set.
- `TransactionManager::abort(&mut self, txn_id, pool) -> Result<(), TxnError>` -
  aborts `txn_id`: undoes every write it made (via
  `storage::recovery::undo_transaction`, the same routine crash recovery's
  own Undo pass uses), then removes it from the active set. The undo chain
  starts from `pool.last_lsn(txn_id)` — the log's own record of this
  transaction's most recent append — rather than any value cached at
  `begin` time, since writes made through `PageGuard::write` append
  directly to the log without going back through this manager.
- `TransactionManager::get(&self, txn_id) -> Result<&Transaction, TxnError>` -
  looks up the current state of an active transaction.
- `TransactionManager::active_snapshot(&self, pool) -> Vec<(TxnId, Lsn)>` -
  every currently active transaction's id and most recent LSN, for a
  checkpoint's Active Transaction Table snapshot.

## Usage Example

```rust
use txn::{IsolationLevel, TransactionManager};

let mut manager = TransactionManager::new(highest_txn_id_seen);
let txn_id = manager.begin(&pool, IsolationLevel::ReadCommitted)?;
manager.commit(txn_id, &pool)?;
```

`engine::Database` owns one `TransactionManager`, seeded at startup from
whatever `storage::recovery::recover` reports as the highest `TxnId` it
saw in the log; every statement's execution begins and commits (or
aborts) a transaction through this one manager, and `write_checkpoint`
reads its `active_snapshot` to build a checkpoint's Active Transaction
Table.
