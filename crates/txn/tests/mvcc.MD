# mvcc

Exercises `VersionChain::visible_version` directly, proving the
visibility rule it implements: newest-first traversal, an uncommitted
version is never visible, a committed version becomes visible at its
`begin_ts` and stops being visible at its `end_ts`.

## Key Components

- `entry(creator, begin_ts, end_ts) -> VersionEntry` - private. Builds a
  `VersionEntry` with empty `tuple_bytes` for tests that only care about
  the visibility fields.
- `uncommitted_entry_is_never_visible` - a `VersionEntry` with
  `begin_ts: None` is invisible at every `read_ts` tried, including one
  far in the future.
- `committed_entry_is_visible_at_or_after_begin_ts` - a single committed,
  never-superseded entry is invisible one tick before its `begin_ts` and
  visible at and after it.
- `two_committed_entries_pick_the_newest_covering_begin_ts` - a chain
  holding an older entry (`begin_ts` 5, superseded at `end_ts` 10) and a
  newer one (`begin_ts` 10, still live) returns the older entry for every
  `read_ts` in `[5, 10)`, the newer entry from `read_ts` 10 onward, and
  `None` before 5.
- `entry_superseded_at_or_before_read_ts_is_invisible` - an entry whose
  `end_ts` is at or before `read_ts` is invisible, matching the "not a
  half-open window past its end" rule; one tick before `end_ts` it is
  still visible.
- `empty_chain_returns_none` - a chain with no versions returns `None`
  for any `read_ts`.

## Usage Example

```rust
use common::TxnId;
use txn::{VersionChain, VersionEntry};

let mut chain = VersionChain::new();
chain.push(VersionEntry {
    creator_txn_id: TxnId(1),
    begin_ts: Some(5),
    end_ts: None,
    tuple_bytes: vec![],
});
assert!(chain.visible_version(4).is_none());
assert!(chain.visible_version(5).is_some());
```

Run as part of `cargo test --workspace`; this is the crate's proof that
`VersionChain::visible_version` implements the snapshot-isolation
visibility rule correctly, ahead of `VersionStore` (M10.3's next subtask)
wiring version chains up to real `Rid`s.
