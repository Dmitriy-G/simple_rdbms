# version_store

The MVCC version chain for every logical row in the database, keyed by
`Rid` and shared across every session, sitting alongside `mvcc.rs`'s
per-row `VersionChain`/`VersionEntry` types the way `LockManager`
(`lock_manager.rs`) sits alongside the resources it locks. `TransactionManager`
(`manager.MD`) owns one `VersionStore` for the lifetime of the process and
exposes it via `TransactionManager::version_store`, mirroring
`lock_manager`, so executors can be handed `Arc<VersionStore>` clones to
record inserts and consult visibility through instead of locking rows
under snapshot isolation — see the closing note below for what still
isn't wired up.

## Key Components

- `VersionStore` - `Mutex<HashMap<Rid, VersionChain>>`, `Send + Sync`.
  A `Rid` with no entry means the row predates this process's MVCC
  bookkeeping (or has never been touched) and is visible to everyone;
  only rows a running process has itself inserted, committed or aborted
  carry a chain.
- `VersionStore::new() -> Self` - an empty store.
- `VersionStore::record_insert(&self, txn_id, rid, tuple_bytes)` - pushes
  a new, uncommitted `VersionEntry` (`begin_ts: None`, `end_ts: None`)
  onto `rid`'s chain, creating the chain if this is the row's first
  recorded version. Called by the insert executor right after the heap
  assigns the `Rid`, so the row exists on disk but stays invisible to
  every other transaction until its creator commits.
- `VersionStore::commit_versions(&self, txn_id, commit_ts)` - calls
  `VersionChain::mark_committed(txn_id, commit_ts)` on every chain in the
  store, making `txn_id`'s versions visible to readers with a snapshot at
  or after `commit_ts`. Called by `TransactionManager::commit` after the
  commit record is durable and before locks are released, so a version
  never becomes visible ahead of its own durability.
- `VersionStore::abort_versions(&self, txn_id)` - calls
  `VersionChain::remove_creator(txn_id)` on every chain, dropping the
  chain entirely once it is left empty so an aborted-then-abandoned `Rid`
  does not linger in the map forever. Called by `TransactionManager::abort`.
  Open question, not yet resolved: dropping a `Rid`'s only chain this way
  makes it indistinguishable, to `is_visible`, from a `Rid` this store has
  never heard of - both read as "visible to everyone" - so a `Rid` whose
  sole version was just aborted is briefly visible again until something
  else records a new version there. This is harmless as long as
  `is_visible` is only ever asked about a `Rid` a scan found a live tuple
  for (the aborting transaction's own undo removes that tuple from the
  heap before this window matters), but it is a real gap in the type on
  its own terms; see `tests/version_store.MD`'s note on
  `aborted_insert_does_not_resurface_once_another_transaction_commits_a_new_version`
  for what a test of just this store cannot yet prove.
- `VersionStore::is_visible(&self, rid, read_ts) -> bool` - `true` when
  `rid` has no chain, otherwise delegates to
  `VersionChain::visible_version(read_ts).is_some()`. Answers "would any
  reader with this snapshot see this row," with no notion of who is
  asking - see `is_visible_to` for the version an actual reader needs.
- `VersionStore::is_visible_to(&self, rid, reader_txn_id, read_ts) -> bool` -
  like `is_visible`, but via `VersionChain::visible_version_for`: a
  version `reader_txn_id` itself created is visible even if nobody has
  committed it yet. Called by a scan executor under snapshot isolation
  (`executor::operators::seq_scan.MD`/`index_scan.MD`) to decide whether
  to yield a row it read from the heap - always this method, not
  `is_visible`, since a transaction that cannot see its own uncommitted
  `INSERT` inside `BEGIN; INSERT ...; SELECT ...` would be a correctness
  bug, not merely a stricter isolation level.

Scanning every chain in `commit_versions`/`abort_versions` rather than
tracking each transaction's touched `Rid`s separately is a chosen
simplicity/cost trade-off: a transaction typically dirties far fewer rows
than the table holds live chains for, but the store has no other way yet
to look up "every `Rid` this `txn_id` created a version for" without
threading that set through the caller. Revisit if profiling shows commit
latency scaling with total chain count rather than a transaction's own
write set.

## Usage Example

```rust
use common::{PageId, Rid, TxnId};
use txn::VersionStore;

let store = VersionStore::new();
let rid = Rid::new(PageId(3), 0);

store.record_insert(TxnId(1), rid, b"row bytes".to_vec());
assert!(!store.is_visible(rid, 10));

store.commit_versions(TxnId(1), 10);
assert!(store.is_visible(rid, 10));
```

`executor::ExecutorContext` carries a `version_store: &'a VersionStore`
field: `InsertExecutor` calls `record_insert` right after the heap
assigns a new row's `Rid`, and `SeqScanExecutor`/`IndexScanExecutor` call
`is_visible_to` under `txn::IsolationLevel::SnapshotIsolation` instead of
taking a shared row lock (`executor::operators::insert.MD`/
`seq_scan.MD`/`index_scan.MD`).
