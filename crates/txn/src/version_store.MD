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
- `VersionStore::record_insert(&self, txn_id, rid)` - pushes a new,
  uncommitted `VersionEntry` (`begin_ts: None`, `end_ts: None`) onto
  `rid`'s chain, creating the chain if this is the row's first recorded
  version. Carries no copy of the row's bytes - visibility is a boolean,
  and the scan executors already read the row's bytes back from the heap,
  so a second copy here would just be a second copy of the whole database
  living in memory for the life of the process. Called by the insert
  executor from the closure it
  passes to `storage::heap::TableHeap::insert_tuple_with`
  (`storage::heap.MD`, `executor::operators::insert.MD`), which runs
  while the row's page latch is still held - so the row's bytes become
  readable in the heap and this version entry exist as one indivisible
  step, and no reader can ever observe the row with no chain at all.
- `VersionStore::commit_versions(&self, txn_id, commit_ts)` - calls
  `VersionChain::mark_committed(txn_id, commit_ts)` on every chain in the
  store, making `txn_id`'s versions visible to readers with a snapshot at
  or after `commit_ts`. Called by `TransactionManager::commit` after the
  commit record is durable and before locks are released, so a version
  never becomes visible ahead of its own durability.
- `VersionStore::abort_versions(&self, txn_id)` - calls
  `VersionChain::remove_creator(txn_id)` on every chain, but keeps every
  chain in the map, empty or not - it never removes an entry. Called by
  `TransactionManager::abort`. This matters because "no entry in the map"
  is `is_visible`/`is_visible_to`'s signal that a `Rid` predates this
  process's MVCC bookkeeping and is visible to everyone; if an aborted-
  and-emptied chain were dropped instead, a `Rid` whose only recorded
  version was just aborted would become indistinguishable from a `Rid`
  never touched at all, and would read as visible again until something
  else recorded a new version there. Keeping the (now empty) chain means
  `VersionChain::visible_version`/`visible_version_for` correctly return
  `None` for it, so both lookup methods return `false`. The store has no
  version GC yet, so an aborted-only `Rid` keeps an empty chain in the map
  forever - accepted for now; revisit if profiling shows the map growing
  unboundedly with aborted transactions.
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
- `VersionStore::chain_count(&self) -> usize` - the number of chains
  currently in the map, `#[cfg(any(test, feature = "test-util"))]` in the
  style of `LockManager::held_lock_count` (`lock_manager.MD`). Test-only:
  it exists so a test can pin the store's growth rule (one chain per
  distinct `Rid` ever recorded, growing forever since there is no GC -
  see `abort_versions` above) rather than only inferring it indirectly
  through visibility checks.

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

store.record_insert(TxnId(1), rid);
assert!(!store.is_visible(rid, 10));

store.commit_versions(TxnId(1), 10);
assert!(store.is_visible(rid, 10));
```

`executor::ExecutorContext` carries a `version_store: &'a VersionStore`
field: `InsertExecutor` calls `record_insert` from the callback it hands
to `TableHeap::insert_tuple_with`, while the new row's page latch is
still held, and `SeqScanExecutor`/`IndexScanExecutor` call
`is_visible_to` under `txn::IsolationLevel::SnapshotIsolation` instead of
taking a shared row lock (`executor::operators::insert.MD`/
`seq_scan.MD`/`index_scan.MD`).
