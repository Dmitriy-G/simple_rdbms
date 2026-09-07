# version_store

The MVCC version chain for every logical row in the database, keyed by
`Rid` and shared across every session, sitting alongside `mvcc.rs`'s
per-row `VersionChain`/`VersionEntry` types the way `LockManager`
(`lock_manager.rs`) sits alongside the resources it locks. `TransactionManager`
(`manager.MD`) owns one `VersionStore` for the lifetime of the process and
exposes it via `TransactionManager::version_store`, mirroring
`lock_manager`, so executors can be handed `Arc<VersionStore>` clones to
record inserts and consult visibility through instead of locking rows
under snapshot isolation — see the closing paragraph below for what
bounds the map's size, and the paragraph after the usage example for
which executors actually call it.

## Key Components

- `VersionStore` - a single `Mutex<StoreState>`, `Send + Sync`, where
  `StoreState` holds `chains: HashMap<Rid, VersionChain>`,
  `write_sets: HashMap<TxnId, Vec<Rid>>` and
  `prune_candidates: BTreeMap<u64, Vec<Rid>>` behind the one lock, so a
  concurrent `record_insert` can never leave any of the three disagreeing
  about which `Rid`s a transaction touched or which `Rid`s are due for a
  recheck. A `Rid` with no chain entry means the row predates this
  process's MVCC bookkeeping (or has never been touched) and is visible
  to everyone; only rows a running process has itself inserted, committed
  or aborted carry a chain. `write_sets` is purely bookkeeping - it exists
  so `commit_versions`/`abort_versions` can find "every `Rid` this
  `txn_id` touched" without scanning `chains`, and an entry is removed
  from it the moment that transaction commits or aborts. `prune_candidates`
  is the analogous index for `prune` below: every `Rid` that might have
  become droppable, keyed by the timestamp at or above which it is safe to
  recheck, so `prune` can find "every candidate due at or before this
  watermark" without scanning `chains` either.
- `VersionStore::new() -> Self` - an empty store.
- `VersionStore::record_insert(&self, txn_id, rid)` - pushes a new,
  uncommitted `VersionEntry` (`begin_ts: None`, `end_ts: None`) onto
  `rid`'s chain, creating the chain if this is the row's first recorded
  version, and also records `rid` in `txn_id`'s write set. Carries no
  copy of the row's bytes - visibility is a boolean, and the scan
  executors already read the row's bytes back from the heap, so a second
  copy here would just be a second copy of the whole database living in
  memory for the life of the process. Called by the insert executor from
  the closure it passes to `storage::heap::TableHeap::insert_tuple_with`
  (`storage::heap.MD`, `executor::operators::insert.MD`), which runs
  while the row's page latch is still held - so the row's bytes become
  readable in the heap and this version entry exist as one indivisible
  step, and no reader can ever observe the row with no chain at all.
- `VersionStore::commit_versions(&self, txn_id, commit_ts)` - looks up
  and removes `txn_id`'s write set, calls
  `VersionChain::mark_committed(txn_id, commit_ts)` on only the chains it
  names, making `txn_id`'s versions visible to readers with a snapshot at
  or after `commit_ts`, then enrols every one of those `Rid`s as a prune
  candidate under `commit_ts`: once the watermark reaches `commit_ts`,
  each is provably visible to everyone and worth rechecking against
  `VersionChain::is_prunable`. A `txn_id` with no write set (nothing
  recorded) is a no-op. Called by `TransactionManager::commit` after the
  commit record is durable and before locks are released, so a version
  never becomes visible ahead of its own durability. Cost scales with the
  transaction's own write set rather than with every chain the store has
  ever held.
- `VersionStore::abort_versions(&self, txn_id, next_ts)` - looks up and
  removes `txn_id`'s write set, calls `VersionChain::remove_creator(txn_id)`
  on only the chains it names, but keeps every one of those chains in the
  map, empty or not - it never removes a chain entry itself. Every `Rid`
  whose chain `VersionChain::is_empty` afterward is enrolled as a prune
  candidate under `next_ts`, the caller's second argument: the next
  timestamp `TransactionManager` would issue at the moment of the abort,
  not one it has actually handed to any transaction
  (`crates/txn/src/manager.MD`'s `abort` entry explains why that specific
  value is the earliest safe recheck point). Called by
  `TransactionManager::abort`. Retaining the chain here (rather than
  dropping it on the spot) matters because "no entry in the map" is
  `is_visible`/`is_visible_to`'s signal that a `Rid` predates this
  process's MVCC bookkeeping and is visible to everyone; if an aborted-
  and-emptied chain were dropped here, a reader could still be holding a
  copy of the row's bytes it read from the heap before
  `TransactionManager::abort`'s undo ran (`crates/txn/src/manager.rs:86-97`,
  `crates/executor/src/operators/seq_scan.rs:38-51`), and would then find
  no chain at all for that `Rid` and treat the row as visible to
  everyone. Keeping the (now empty) chain means
  `VersionChain::visible_version`/`visible_version_for` correctly return
  `None` for it, so both lookup methods return `false`, until `prune`
  drops it once it is provably safe. This is an ADR-level rule rather than
  a local choice: `docs/adr/0013-version-identity-and-lifetime.md`
  ("Lifetime") is where it is recorded, not this method - this method
  itself never removes a chain entry, empty or not.
- `VersionStore::prune(&self, watermark)` - drains every `prune_candidates`
  key at or below `watermark`, and for each `Rid` filed under one,
  re-checks `VersionChain::is_prunable(watermark)` against the chain's
  *current* state before removing it from `chains`. The recheck matters
  because a `Rid` can be enrolled once (say, when an abort empties it)
  and then gain an unrelated, not-yet-prunable version before the
  watermark ever reaches that enrollment's timestamp - a later insert
  reusing the same physical slot once the heap reclaims it. Trusting the
  enrollment instead of rechecking would drop a chain a live snapshot
  still needs. Never scans `chains` itself: cost is proportional to the
  candidates due, not to the chains the store has ever held. Called by
  `TransactionManager::commit` and `TransactionManager::abort`, both
  after the transaction leaves the active set, with
  `TransactionManager::oldest_active_read_ts` as the watermark - there is
  no timer or background thread; pruning only ever runs where the
  watermark can have changed.
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
  it exists so a test can pin the store's actual growth rule rather than
  only inferring it indirectly through visibility checks.

`record_insert` adds a chain entry for every insert, committed or not,
and neither `commit_versions` nor `abort_versions` removes one on the
spot - the former only stamps `begin_ts`, the latter only empties the
chain, for the reason given above. Both cost only as much as the
committing or aborting transaction's own write set, via the `write_sets`
index above, rather than scanning every chain the store has ever held.
What actually bounds the map's size is `prune`, called from
`TransactionManager::commit`/`abort` against
`TransactionManager::oldest_active_read_ts`
(`docs/adr/0013-version-identity-and-lifetime.md`, "Lifetime"): a chain
becomes droppable once no present or future reader can still need it, so
the store's steady-state size tracks *live, contended* rows - the ones a
long-running transaction is still holding open - rather than every row
ever inserted since the process started.

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
