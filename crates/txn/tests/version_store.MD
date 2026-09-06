# version_store

Drives `VersionStore` directly through its public API, proving the
`Rid`-keyed bookkeeping it layers on top of `VersionChain` (already
covered in isolation by `mvcc.rs`): an unrecorded row is visible to
everyone, a recorded-but-uncommitted insert is invisible to everyone
except its own creator, a commit makes it visible to everyone from its
commit timestamp onward, and an abort makes it invisible forever
regardless of what commits later.

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
- `aborted_insert_stays_invisible_immediately_after_the_abort` - after
  `record_insert` and `abort_versions` on the same `Rid`, with nothing
  else recorded there, both `is_visible` and `is_visible_to` (even with
  the aborting transaction's own id) return `false` - proving the emptied
  chain `abort_versions` leaves behind in the map is not confused with "no
  chain for this `Rid`," which would read as visible to everyone.
- `aborted_insert_does_not_resurface_once_another_transaction_commits_a_new_version` -
  after `abort_versions` discards the first transaction's entry, a second
  transaction recording and committing its own version of the same `Rid`
  makes it visible only from that second commit's timestamp, not before -
  proving the aborted version was actually removed from the chain rather
  than left behind to confuse a later reader.
- `different_transactions_recording_different_rids_do_not_affect_each_other` -
  committing one `Rid`'s writer and, separately, another `Rid`'s writer at
  a different commit timestamp leaves each row's visibility independent
  of the other's.
- `is_visible_to_shows_the_readers_own_uncommitted_insert` - a
  recorded-but-uncommitted row is visible via `is_visible_to` to its own
  creator's `TxnId` but not to a different one, at the same `read_ts`.
- `is_visible_to_matches_is_visible_for_committed_rows` - once a row is
  committed, `is_visible_to` for a *different* transaction's id agrees
  exactly with plain `is_visible`: invisible one tick before the commit
  timestamp, visible at it.
- `chain_count_tracks_one_chain_per_distinct_rid_recorded` - records and
  commits a version for 20 distinct `Rid`s and checks `chain_count()`
  equals 20, then records and commits one more version against an
  already-seen `Rid` and checks the count is unchanged - pinning the
  store's actual growth rule (one chain per distinct `Rid` ever recorded,
  regardless of how many versions or transactions touch it, and never
  shrinking - there is no GC) so a future change to that rule fails this
  test loudly instead of only being noticed as a memory-growth surprise.

## Usage Example

```rust
use common::{PageId, Rid, TxnId};
use txn::VersionStore;

let store = VersionStore::new();
let rid = Rid::new(PageId(0), 1);

store.record_insert(TxnId(1), rid);
assert!(!store.is_visible(rid, 0));

store.commit_versions(TxnId(1), 5);
assert!(store.is_visible(rid, 5));
```

Run as part of `cargo test --workspace`; this is the crate's proof that
`VersionStore`'s per-`Rid` map correctly delegates to and isolates
`VersionChain`s from each other, independent of `TransactionManager`
(`../src/manager.MD`) actually allocating the timestamps `commit_versions`,
`is_visible`, and `is_visible_to` are driven with in a running
transaction - `tests/lifecycle.rs` covers that integration.
`chain_count_tracks_one_chain_per_distinct_rid_recorded` needs
`VersionStore::chain_count`, gated `#[cfg(any(test, feature =
"test-util"))]`: this file links against the crate's normal (non-`--cfg
test`) build like any integration test, so `../Cargo.toml`'s
self-referencing `txn` dev-dependency with `test-util` enabled is what
makes it visible here, the same way `crates/storage/Cargo.toml` does for
`storage`'s own `tests/`.
