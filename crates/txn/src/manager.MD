# manager

Owns the lifecycle of every transaction: assigning ids, tracking active
transactions, and driving commit/abort — which means writing the
corresponding log record(s) and, for abort, undoing whatever the
transaction had already written.

## Key Components

- `TransactionManager` - the manager. Holds `active`, a map from `TxnId`
  to `Transaction` for every currently in-flight transaction,
  `next_txn_id`, the counter handing out fresh ids, and `lock_manager`,
  the `Arc<LockManager>` (`lock_manager.MD`) every executor locks and
  unlocks through - one instance, shared with every caller via
  `TransactionManager::lock_manager`, so a lock taken on behalf of one
  transaction is visible to every other transaction checking the same
  resource.
- `TransactionManager::new(highest_seen) -> Self` - creates a manager with
  no active transactions and a fresh, empty `LockManager`, handing out ids
  starting past `highest_seen` — the highest `TxnId` `recovery::recover`
  observed anywhere in the log, or `None` for a log that has never
  recorded a real transaction. Seeding from this rather than always
  starting at `0` is what stops a freshly opened database from ever
  reusing an id the log has already used: a reused id's new `Begin` would
  otherwise chain its `prev_lsn` onto an unrelated transaction's own log
  records, and — if that old id was a "phantom winner" left by a crash
  between a `Commit` and its `End` — could even make a brand new,
  never-committed transaction look already committed.
- `TransactionManager::lock_manager(&self) -> &Arc<LockManager>` - the
  shared lock manager every transaction this `TransactionManager` tracks
  locks and unlocks through. `executor::ExecutorContext` carries a clone
  of this `Arc` so every operator locks against the same instance
  `commit`/`abort` release through.
- `TransactionManager::begin(&mut self, pool, isolation_level) -> Result<TxnId, TxnError>` -
  `#[tracing::instrument(skip_all, fields(txn_id, isolation = ?isolation_level))]`,
  recording `txn_id` once it's assigned (it isn't known until inside the
  function body, so the field starts `Empty` and is filled via
  `Span::current().record`). Begins a new transaction at the given
  isolation level: assigns it the next id, logs a `Begin` record, and
  tracks it as active. Returns its id. A debug assertion checks the
  freshly assigned id exceeds every already-active id, catching an id
  counter that was not seeded past every id the log has ever used.
- `TransactionManager::commit(&mut self, txn_id, pool) -> Result<(), TxnError>` -
  `#[tracing::instrument(skip_all, fields(txn_id = txn_id.0))]`. Commits
  `txn_id`: logs a `Commit` record and force-flushes the log up to it (so
  an acknowledged commit can never be lost to a crash), logs `End`,
  removes it from the active set, then releases every lock it held via
  `lock_manager.release_all` (strict two-phase locking: the whole set
  releases at once, never before), then increments
  `transactions_committed_total` (a `metrics` counter - see
  `docs/ROADMAP.md`'s M13 entry).
- `TransactionManager::abort(&mut self, txn_id, pool) -> Result<(), TxnError>` -
  `#[tracing::instrument(skip_all, fields(txn_id = txn_id.0))]`, and emits
  a `warn!` as soon as `txn_id` is confirmed active - every abort, crash
  recovery's own Undo pass aside, is an operator-relevant event by
  definition (see CLAUDE.md's logging section). Aborts `txn_id`: undoes
  every write it made (via `storage::recovery::undo_transaction`, the
  same routine crash recovery's own Undo pass uses), removes it from the
  active set, then releases every lock it held via
  `lock_manager.release_all`, same as `commit`. The undo chain starts from
  `pool.last_lsn(txn_id)` — the log's own record of this transaction's
  most recent append — rather than any value cached at `begin` time, since
  writes made through `PageGuard::write` append directly to the log
  without going back through this manager. Increments
  `transactions_aborted_total` once the undo chain (and the log records it
  wrote) is complete.
- `TransactionManager::get(&self, txn_id) -> Result<&Transaction, TxnError>` -
  looks up the current state of an active transaction.
- `TransactionManager::active_snapshot(&self, pool) -> Vec<(TxnId, Lsn)>` -
  every currently active transaction's id and most recent LSN, for a
  checkpoint's Active Transaction Table snapshot.
- `TransactionManager::earliest_active_begin_lsn(&self) -> Option<Lsn>` -
  the smallest `begin_lsn` among every currently active transaction, or
  `None` if none are active. Deliberately each transaction's *first* LSN,
  not its most recent one the way `active_snapshot` reports - `write_checkpoint`
  uses this (not `active_snapshot`) to bound how far back the write-ahead
  log can safely be truncated, since undo needs every record from a
  transaction's `Begin` onward, and this engine's buffer pool can flush an
  active transaction's early writes to disk well before it commits or
  aborts (`docs/adr/0011-segmented-write-ahead-log.md`).

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
