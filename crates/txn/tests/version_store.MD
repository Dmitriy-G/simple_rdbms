# version_store

Drives `VersionStore` directly through its public API, proving the
`Rid`-keyed bookkeeping it layers on top of `VersionChain` (already
covered in isolation by `mvcc.rs`): an unrecorded row is visible to
everyone, a recorded-but-uncommitted insert is invisible to everyone, a
commit makes it visible from its commit timestamp onward, and an abort
makes it invisible forever regardless of what commits later.

## Key Components

- `rid(slot) -> Rid` - private. Builds a `Rid` on a fixed `PageId(0)` for
  tests that only care about distinguishing rows.
- `unknown_rid_is_visible_at_any_read_ts` - a `Rid` the store has never
  seen is visible at `read_ts` 0 and at a large `read_ts`, since the
  store has no chain to consult.
- `recorded_but_uncommitted_insert_is_invisible` - after
  `record_insert` alone (no `commit_versions`), the row is invisible at
  every `read_ts` tried.
- `committed_insert_is_visible_at_and_after_its_commit_ts` - after
  `commit_versions(txn, 5)`, the row is invisible at `read_ts` 4 and
  visible at 5 and 6.
- `aborted_insert_does_not_resurface_once_another_transaction_commits_a_new_version` -
  after `abort_versions` discards the first transaction's entry, a second
  transaction recording and committing its own version of the same `Rid`
  makes it visible only from that second commit's timestamp, not before -
  proving the aborted version was actually removed from the chain rather
  than left behind to confuse a later reader. Deliberately not covered:
  whether the `Rid` is visible in the gap between the abort and that
  second `record_insert`, once the abort has dropped the chain to empty
  and removed it from the map - `VersionStore::is_visible`'s documented
  "no chain for this `Rid`" case is indistinguishable from "this exact
  `Rid` had its only version aborted," so that gap reads as visible
  rather than invisible. See `../src/version_store.MD`'s note on
  `abort_versions` for the open question.
- `different_transactions_recording_different_rids_do_not_affect_each_other` -
  committing one `Rid`'s writer and, separately, another `Rid`'s writer at
  a different commit timestamp leaves each row's visibility independent
  of the other's.

## Usage Example

```rust
use common::{PageId, Rid, TxnId};
use txn::VersionStore;

let store = VersionStore::new();
let rid = Rid::new(PageId(0), 1);

store.record_insert(TxnId(1), rid, b"row".to_vec());
assert!(!store.is_visible(rid, 0));

store.commit_versions(TxnId(1), 5);
assert!(store.is_visible(rid, 5));
```

Run as part of `cargo test --workspace`; this is the crate's proof that
`VersionStore`'s per-`Rid` map correctly delegates to and isolates
`VersionChain`s from each other, ahead of `TransactionManager` (M10.3's
next subtask) allocating the timestamps `commit_versions` and
`is_visible` are driven with in a running transaction.
