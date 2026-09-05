# mvcc

The multi-version concurrency control types used for snapshot isolation:
one version of a tuple, and the chain of versions for a single logical
row.

## Key Components

- `VersionEntry` - one version of a tuple in an MVCC version chain: the
  tuple's bytes as of some transaction's write, plus the visibility
  window (in timestamp terms) during which that version is the one a
  reader should see. `creator_txn_id` is the transaction that created this
  version. `begin_ts` is the timestamp this version became visible (its
  creator's commit timestamp), or `None` if the creator has not yet
  committed. `end_ts` is the timestamp this version stopped being visible
  (the timestamp of whatever transaction superseded it), or `None` if it
  is still the current version. `tuple_bytes` is the tuple's encoded bytes
  as of this version.
- `VersionChain` - the chain of historical versions for a single logical
  row, newest first. A reader under snapshot isolation walks this chain to
  find the newest version whose `begin_ts` is at or before its own
  snapshot timestamp.
- `VersionChain::new() -> Self` - creates an empty version chain.
- `VersionChain::push(&mut self, entry)` - prepends a newly created
  version to the chain.
- `VersionChain::visible_version(&self, read_ts) -> Option<&VersionEntry>` -
  finds the version visible to a reader with snapshot timestamp `read_ts`:
  the newest version whose `begin_ts` is at or before `read_ts` and whose
  `end_ts` is either absent or after it. Since `push` inserts at index 0,
  `versions` is newest-first, so this returns the first entry (in
  iteration order) that qualifies. A version whose `begin_ts` is `None`
  (its creator has not committed yet) never qualifies, regardless of
  `read_ts`. Returns `None` when no entry qualifies, including on an
  empty chain.
- `VersionChain::mark_committed(&mut self, txn_id, commit_ts)` - sets
  `begin_ts = Some(commit_ts)` on every entry whose `creator_txn_id` is
  `txn_id`, making that transaction's versions visible to readers with a
  snapshot at or after `commit_ts`. Called by
  `version_store::VersionStore::commit_versions` once a transaction's
  commit record is durable.
- `VersionChain::remove_creator(&mut self, txn_id)` - drops every entry
  whose `creator_txn_id` is `txn_id`, undoing an aborted transaction's
  writes to this row. Called by
  `version_store::VersionStore::abort_versions`.
- `VersionChain::is_empty(&self) -> bool` - true when the chain holds no
  versions, so its caller (`VersionStore`) knows to drop the chain
  entirely rather than keep an empty one keyed by a `Rid`.

## Usage Example

```rust
use txn::{VersionChain, VersionEntry};

let mut chain = VersionChain::new();
chain.push(VersionEntry {
    creator_txn_id,
    begin_ts: Some(1),
    end_ts: None,
    tuple_bytes: vec![],
});
let visible = chain.visible_version(1);
```

Not yet wired into `executor` or `storage::heap`: this crate provides the
version-chain type ahead of snapshot-isolation reads actually consulting
it instead of reading a table heap's current tuple directly.
