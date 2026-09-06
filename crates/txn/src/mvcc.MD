# mvcc

The multi-version concurrency control types used for snapshot isolation:
one version of a tuple, and the chain of versions for a single logical
row.

## Key Components

- `VersionEntry` - one version of a tuple in an MVCC version chain: not
  the tuple's bytes themselves, only the visibility window (in timestamp
  terms) during which that version is the one a reader should see -
  visibility is a boolean, and a scan executor already reads the row's
  actual bytes back from the heap (`storage::heap.MD`), so this type
  carries no second copy of them. `creator_txn_id` is the transaction
  that created this version. `begin_ts` is the timestamp this version
  became visible (its creator's commit timestamp), or `None` if the
  creator has not yet committed. `end_ts` is the timestamp this version
  stopped being visible (the timestamp of whatever transaction superseded
  it), or `None` if it is still the current version.
- `VersionChain` - the chain of historical versions for a single logical
  row, newest first. A reader under snapshot isolation walks this chain to
  find the newest version whose `begin_ts` is at or before its own
  snapshot timestamp.
- `VersionChain::new() -> Self` - creates an empty version chain.
- `VersionChain::push(&mut self, entry)` - prepends a newly created
  version to the chain.
- `VersionChain::visible_version(&self, read_ts) -> Option<&VersionEntry>` -
  finds the version visible to *any* reader with snapshot timestamp
  `read_ts`, regardless of who created it: the newest version whose
  `begin_ts` is at or before `read_ts` and whose `end_ts` is either absent
  or after it. Since `push` inserts at index 0, `versions` is newest-first,
  so this returns the first entry (in iteration order) that qualifies. A
  version whose `begin_ts` is `None` (its creator has not committed yet)
  never qualifies here, regardless of `read_ts` - see
  `visible_version_for` for the one case where an uncommitted version must
  still be visible. Returns `None` when no entry qualifies, including on
  an empty chain.
- `VersionChain::visible_version_for(&self, reader_txn_id, read_ts) ->
  Option<&VersionEntry>` - like `visible_version`, except an entry created
  by `reader_txn_id` qualifies unconditionally, committed or not. A
  transaction must always see its own writes even before it commits them -
  otherwise `BEGIN; INSERT ...; SELECT ...` would not show the row it just
  inserted, since nothing else has committed it yet. Delegates the
  everyone-else case to the same rule `visible_version` uses (the private
  `is_globally_visible` helper both share).
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
  versions. Called by `VersionStore::abort_versions` to decide which
  `Rid`s an abort emptied and must therefore enrol as prune candidates -
  see `version_store.MD`'s `abort_versions` entry. Kept as a
  general-purpose query on the chain's state as well.
- `VersionChain::is_prunable(&self, watermark) -> bool` - the predicate
  `docs/adr/0013-version-identity-and-lifetime.md` ("Lifetime") defines:
  `true` when the chain is empty, or when its newest version (`versions[0]`,
  since `push` prepends) is committed (`begin_ts` is `Some(ts)`) at or
  before `watermark` and has no `end_ts`. A chain whose newest version has
  `begin_ts == None` (uncommitted) or `end_ts` set is never prunable -
  the latter is M14's territory, once a version can be superseded by a
  `DELETE`/`UPDATE` rather than only by another transaction's abort.
  Deliberately looks only at the newest version: an older, already-
  superseded entry's own `begin_ts`/`end_ts` never matters once a newer
  one exists, since no reader can still resolve to it. Called by
  `VersionStore::prune`, which re-checks it against the current watermark
  for every candidate rather than trusting whatever fact caused a `Rid` to
  be enrolled as a candidate in the first place - a later transaction may
  have pushed a new version onto the same `Rid` since then
  (`crates/txn/tests/lifecycle.rs`'s prune tests cover this).
- `is_globally_visible(entry, read_ts) -> bool` - private. The shared
  "would any reader with this snapshot see this version" predicate:
  `begin_ts` at or before `read_ts` and (`end_ts` absent or after
  `read_ts`). `visible_version` uses it as its whole rule; `visible_version_for`
  uses it only for entries some other transaction created.

## Usage Example

```rust
use txn::{VersionChain, VersionEntry};

let mut chain = VersionChain::new();
chain.push(VersionEntry {
    creator_txn_id,
    begin_ts: Some(1),
    end_ts: None,
});
let visible = chain.visible_version(1);
```

`version_store::VersionStore` is the only caller: every `VersionChain` a
running process holds lives inside one of its per-`Rid` map entries, and
`executor`'s scan operators reach this type only indirectly, through
`VersionStore::is_visible`/`is_visible_to`.
