# manager

Owns the lifecycle of every transaction: assigning ids, tracking active
transactions, and driving commit/abort — which means writing the
corresponding log record(s) and, for abort, undoing whatever the
transaction had already written.

## Key Components

- `TransactionManager` - the manager. Holds `active`, a map from `TxnId`
  to `Transaction` for every currently in-flight transaction,
  `next_txn_id`, the counter handing out fresh ids, `lock_manager`,
  the `Arc<LockManager>` (`lock_manager.MD`) every executor locks and
  unlocks through - one instance, shared with every caller via
  `TransactionManager::lock_manager`, so a lock taken on behalf of one
  transaction is visible to every other transaction checking the same
  resource - `version_store`, the `Arc<VersionStore>` (`version_store.MD`)
  every executor records inserts into and reads visibility from under
  snapshot isolation, shared the same way, and `next_ts`, the counter
  behind `next_timestamp` below.
- `TransactionManager::new(highest_seen) -> Self` - creates a manager with
  no active transactions, a fresh, empty `LockManager` bounded by
  `common::DbConfig::DEFAULT_LOCK_WAIT_TIMEOUT_MS`, a fresh, empty
  `VersionStore`, and its timestamp counter at `0`, handing out ids
  starting past `highest_seen` — the highest `TxnId` `recovery::recover`
  observed anywhere in the log, or `None` for a log that has never
  recorded a real transaction. Seeding from this rather than always
  starting at `0` is what stops a freshly opened database from ever
  reusing an id the log has already used: a reused id's new `Begin` would
  otherwise chain its `prev_lsn` onto an unrelated transaction's own log
  records, and — if that old id was a "phantom winner" left by a crash
  between a `Commit` and its `End` — could even make a brand new,
  never-committed transaction look already committed.
- `TransactionManager::with_lock_wait_timeout(highest_seen, lock_wait_timeout_ms) -> Self` -
  the same as `new`, except the `LockManager` it builds is bounded by
  `lock_wait_timeout_ms` instead of the default
  (`LockManager::with_timeout`, `lock_manager.MD`), `0` meaning wait
  forever. `engine::runtime::EngineShared::open` calls this with
  `DbConfig::lock_wait_timeout_ms` so the configured bound actually
  reaches the lock manager, the same way it threads
  `idle_in_transaction_timeout_ms` into `EngineShared` itself
  (`docs/adr/0012-bounded-lock-waits.md`). `new` is kept, calling this
  with the default, so every existing caller that doesn't care about the
  bound keeps building unchanged.
- `TransactionManager::lock_manager(&self) -> &Arc<LockManager>` - the
  shared lock manager every transaction this `TransactionManager` tracks
  locks and unlocks through. `executor::ExecutorContext` carries a clone
  of this `Arc` so every operator locks against the same instance
  `commit`/`abort` release through.
- `TransactionManager::version_store(&self) -> &Arc<VersionStore>` - the
  shared MVCC version store, mirroring `lock_manager` exactly:
  `executor::ExecutorContext` carries a clone of this `Arc` so every
  operator records versions into and reads visibility from the same
  instance `commit`/`abort` stamp.
- `next_timestamp(&mut self) -> u64` - private. Returns `next_ts` and
  increments it, the one source of every MVCC timestamp this manager
  hands out - both a transaction's `read_ts` at `begin` and a committing
  transaction's `commit_ts` are drawn from this single monotonic counter,
  so "before" and "after" in commit order and in snapshot order agree.
- `TransactionManager::begin(&mut self, pool, isolation_level) -> Result<TxnId, TxnError>` -
  `#[tracing::instrument(skip_all, fields(txn_id, isolation = ?isolation_level))]`,
  recording `txn_id` once it's assigned (it isn't known until inside the
  function body, so the field starts `Empty` and is filled via
  `Span::current().record`). Begins a new transaction at the given
  isolation level: assigns it the next id, draws its `read_ts` from
  `next_timestamp`, logs a `Begin` record, and tracks it as active.
  Returns its id. A debug assertion checks the freshly assigned id
  exceeds every already-active id, catching an id counter that was not
  seeded past every id the log has ever used.
- `TransactionManager::commit(&mut self, txn_id, pool) -> Result<(), TxnError>` -
  `#[tracing::instrument(skip_all, fields(txn_id = txn_id.0))]`. Commits
  `txn_id`: logs a `Commit` record and force-flushes the log up to it (so
  an acknowledged commit can never be lost to a crash), draws a
  `commit_ts` from `next_timestamp` and calls
  `version_store.commit_versions(txn_id, commit_ts)` — after the flush, so
  a version can never become visible before its commit record is durable
  — logs `End`, removes it from the active set, then calls
  `version_store.prune(self.oldest_active_read_ts())` now that the active
  set (and therefore the watermark) reflects `txn_id` having left it, then
  releases every lock it held via `lock_manager.release_all` (strict
  two-phase locking: the whole set releases at once, never before), then
  calls `lock_manager.prune_finished(self.oldest_active_txn_id())` so the
  lock manager's own bookkeeping of finished transactions stays bounded
  the same way `version_store.prune` bounds version chains, then
  increments `transactions_committed_total` (a `metrics` counter - see
  `docs/ROADMAP.md`'s M13 entry).
- `TransactionManager::abort(&mut self, txn_id, pool) -> Result<(), TxnError>` -
  `#[tracing::instrument(skip_all, fields(txn_id = txn_id.0))]`, and emits
  a `warn!` as soon as `txn_id` is confirmed active - every abort, crash
  recovery's own Undo pass aside, is an operator-relevant event by
  definition (see CLAUDE.md's logging section). Aborts `txn_id`: undoes
  every write it made (via `storage::recovery::undo_transaction`, the
  same routine crash recovery's own Undo pass uses), removes it from the
  active set, calls `version_store.abort_versions(txn_id, self.next_ts)`
  so none of its uncommitted versions can ever become visible - passing
  `next_ts` **before** it is ever consumed by `next_timestamp`, since that
  is the earliest timestamp every transaction that could still be holding
  the undone row's bytes (all of them active at this moment, therefore
  with a `read_ts` below it) is guaranteed to have finished by - then
  calls `version_store.prune(self.oldest_active_read_ts())`, same as
  `commit`, then releases every lock it held via `lock_manager.release_all`,
  then calls `lock_manager.prune_finished(self.oldest_active_txn_id())`,
  same as `commit`.
  The undo chain starts from `pool.last_lsn(txn_id)` — the log's own
  record of this transaction's most recent append — rather than any value
  cached at `begin` time, since writes made through `PageGuard::write`
  append directly to the log without going back through this manager.
  Increments `transactions_aborted_total` once the undo chain (and the log
  records it wrote) is complete.
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
- `TransactionManager::oldest_active_read_ts(&self) -> u64` - the
  smallest `Transaction::read_ts` among every currently active
  transaction, or `self.next_ts` - the timestamp `next_timestamp` would
  hand out next - when none are active. This is the MVCC analogue of
  `earliest_active_begin_lsn` above: a version whose `commit_ts` is at or
  below this watermark can be visible to every present *and future*
  reader, since no snapshot older than the watermark still exists and no
  snapshot taken after it can ever read as of an earlier time, which is
  exactly the condition `VersionStore::prune`
  (`version_store.MD`, `docs/adr/0013-version-identity-and-lifetime.md`
  "Lifetime") drops a chain against. With no transaction active, the
  watermark is `next_ts` rather than, say, the last-issued timestamp,
  because a chain committed at any timestamp strictly below the *next*
  one to be issued is provably safe: nothing still open could have an
  older snapshot, and nothing begun afterward gets one. Called by
  `commit` and `abort` above, each time right before they call `prune`.
- `TransactionManager::oldest_active_txn_id(&self) -> TxnId` - the
  smallest `TxnId` among every currently active transaction, or
  `TxnId(self.next_txn_id)` - the id `begin` would hand out next - when
  none are active. The `TxnId` analogue of `oldest_active_read_ts` above,
  passed to `lock_manager.prune_finished` for the same reason: `begin`
  hands out ids in strictly increasing order, so no transaction still
  active or yet to begin can ever use an id below this watermark, and
  `LockManager::release_all`'s `finished` marker for any older,
  already-finished id can be safely forgotten. Called by `commit` and
  `abort` above, each time right after they call `lock_manager.release_all`.

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
