# btree

A disk-resident B+tree index: internal nodes hold separator keys and
child `PageId`s, leaf nodes hold keys and the `Rid`s of the matching heap
tuples, and leaves are linked for fast ordered range scans. Keys are
opaque `&[u8]` throughout this module - it never constructs a `types::Value`
or knows which column(s) it indexes, so it stays reusable for any column
or combination a future caller encodes into bytes. Correct ordering is the
caller's responsibility: `types::MemcomparableEncode` (see
`memcomparable.MD`) is what turns a `Value` into bytes this module can
compare with plain `[u8]` `Ord` and get the right answer. Milestone M9.1
(this file) covers node layout and the read path (`create`/`open`/`get`/
`range_scan`); M9.2 adds `insert` with leaf and internal splits; M9.3 adds
`leaf_for_start`/`scan_leaf` (the primitives `catalog` and `executor` need
to give this module a place in the query path - see "Key Components"
below) without changing node layout or the split protocol at all; delete
is still `todo!()`, tracked as a later milestone in `docs/ROADMAP.md`.

## Node layout

Every node is one `page::Page`. Bytes `0..4` (`page::CHECKSUM_RANGE`) and
`4..12` (`page::PAGE_LSN_RANGE`) are reserved by `page.rs` for every page
format; a node's own header starts at byte 12, in the same "grow the slot
array down from the header, grow entry payloads up from the end of the
page" shape `heap::SlottedPage` uses, so the two module's on-disk formats
read the same way even though the entries themselves differ:

```
12      node type: 0 = leaf, 1 = internal (1 byte)
13..15  slot count (u16)
15..17  data_used: bytes currently used by entry payloads (u16)
17..21  tail: a PageId, meaning depends on node type (u32, 0 = none)
21..    the slot array: `slot_count` entries of (offset: u16, len: u16),
        4 bytes each, each pointing at one variable-length entry
...     free space
...     entry payloads, packed from the end of the page backwards
```

Every entry, leaf or internal, starts the same way: `key_len: u16`
followed by `key_len` bytes of key. What follows differs by node type:

- **Leaf entry**: key, then a 6-byte `Rid` (`PageId` as `u32`, slot as
  `u16`). `leaf_rid` reads it back.
- **Internal entry**: key, then a 4-byte child `PageId`. `internal_child`
  reads it back.

`entry_key` reads just the leading `key_len`/key prefix, which is why one
binary search (`upper_bound`/`lower_bound`) works unmodified against
either entry shape - everything past the key is opaque to key comparison.

### `tail`: what it means per node type

A leaf's `tail` is its right sibling's `PageId` (`0`/`None` for the
rightmost leaf) - the pointer that lets `get`'s duplicate-key scan and
`BTreeRangeIterator` cross a leaf boundary without walking back up to the
parent.

An internal node's `tail` is its **leftmost child** - not its rightmost,
despite the shared field name. An internal node with `n` entries has
`n + 1` children: reading `tail` as `child[0]`, and each `entries[i]` as
the pair `(key_i, child[i + 1])`, means `entries[i]`'s stored pointer is
the child **to the right** of `key_i`, not the child to its left. The
field is still called `tail` (and stored in the very field `SlottedPage`
would use for a next-page pointer) because it plays the same structural
role a leaf's `tail` does: "the pointer for whatever comes after every
entry this node stores directly." For a leaf, "after every entry" means
the next leaf, to the right. For an internal node built this way, the one
child with no entry of its own to sit next to is child `0`, the leftmost
- so that's what ends up in the shared `tail` field. This choice is not
cosmetic: it is exactly what makes the split rule below match the
convention leaf nodes already use, and reusing the field lets both node
types share the same 21-byte header and the same `Node::rebuild`.

Descent (`child_for_key`) follows directly from this: search for the
right-most entry whose key is `<= K`; if one exists, follow its child;
otherwise `K` is below every separator here, so follow `tail`.

## Splitting

A node overflows when `Node::will_fit` says the new entry's slot plus
payload will not fit - a space check, never an entry-count check, since
keys are variable length and a count threshold would let a page of large
keys overflow its actual byte budget while rejecting a page of small keys
that still has room. `BTreeIndex::insert` builds the new entry's payload
once, tries the fast surgical path first (`Node::insert_at`: shift the
slot sub-array right of the insertion point in one write, write the new
payload, write the new slot, update the header - three writes, no loop),
and only falls back to a split when `will_fit` says no.

### Choosing the split point

`byte_balanced_split_point` walks the full, already-sorted `n + 1`-entry
virtual list (the node's existing entries plus the new one, at its sorted
position) accumulating `SLOT_SIZE + entry.len()` per entry until the
running total reaches half the list's total bytes, and returns that
index. This is deliberately not a count-based midpoint: under ascending
insertion, a naive "split right before the new, always-last entry" rule
would hand the new leaf exactly one entry every time, degenerating into a
chain of near-empty leaves. Balancing by byte mass instead gives a ~50/50
split by size regardless of where in sorted order the overflowing entry
landed, so ascending and descending insertion split exactly as well as a
random one does (`ascending_insert_of_ten_thousand_keys_...` and
`descending_insert_of_ten_thousand_keys_...` in `tests/btree.rs` both
exercise this).

A leaf split must additionally never land in the middle of a run of
duplicate keys. The pushed-up separator is a copy of the new right leaf's
first key (see below); if the chosen split point fell between two entries
that share a key, the separator would equal a key still present in the
*left* node too, violating "every key in child `i` is `< separator i`".
`nearest_clean_boundary` searches outward from the byte-balanced target,
in both directions, for the nearest index where the key changes (index
`0` always qualifies, so the search always terminates), and
`leaf_split_point` uses that adjusted index. `internal_split_point` does
not need this adjustment for the entry it pushes up, since internal
separators are copied from clean leaf-level boundaries in the first
place - but see "Known limitation" below for the one case where a
duplicate separator can still arise.

### Leaf split

Given the merged `n + 1` entries and a clean split index `mid`:

- **Left** (the original page, unchanged `PageId`) keeps entries
  `[0, mid)`.
- **Right** (a freshly allocated page) keeps entries `[mid, n + 1)`.
- `left.tail` becomes the right page's new `PageId`.
- `right.tail` becomes whatever `left.tail` was *before* the split (the
  original next-sibling pointer) - the right leaf inherits the left
  leaf's old place in the sibling chain.
- The separator copied up to the parent is `entry_key` of `right`'s first
  entry. The key **stays in the leaf** - unlike the internal case below,
  nothing is removed, because leaves must hold every key for `get`/
  `range_scan` to find it without consulting internal nodes at all.

### Internal split

Given the merged `n + 1` entries (the node's own entries plus the newly
propagated `(separator, right_child)` pair, inserted at its sorted
position) and a split index `mid` chosen by `internal_split_point`:

- The entry at index `mid` is **removed** from the merged list and pushed
  up to the parent - it lives in neither resulting node.
- **Left** (the original page) keeps entries `[0, mid)`. `left.tail` is
  unchanged: the original tail was already "the child before every entry
  this node keeps," and none of the entries before `mid` moved.
- **Right** (a freshly allocated page) keeps entries `(mid, n + 1)`,
  reindexed to start at `0`; their own child pointers are untouched by
  the reindexing. `right.tail` becomes the *pushed entry's own child
  pointer* - not a survivor of the original tail, and not the pushed
  entry's key (which goes to the parent, not to `right`). This is the
  reassignment the milestone's task description called out as the one
  that silently loses a subtree when done wrong: entry `mid`'s pointer
  was `entries[mid]`'s **right**-child pointer (per the `tail` =
  leftmost-child convention above), i.e. exactly the child that belongs
  immediately to the right of the pushed key and immediately to the left
  of `right`'s first remaining entry - which is precisely what a
  freshly-allocated internal node's own leftmost child (`tail`) should
  be. Verify it against invariant 3 directly: before the split, child
  `mid` (the pushed entry's pointer) held keys in
  `[entries[mid - 1].key, entries[mid].key)`. After the split, `right`'s
  own bound is `[pushed_key, next_separator_or_parent_high)`, and
  `right.tail` must hold everything below `right`'s own first remaining
  separator - exactly the range child `mid` already held. Handing
  anything else to `right.tail` (the original tail, or nothing) would
  either duplicate a subtree the left node still claims or drop one
  entirely.
- The pushed key and `right`'s `PageId` are what propagate to the parent,
  the same shape a leaf split propagates.

### Root split

When propagation empties the descent path (there was no parent to absorb
the pushed entry, because the node that just split was the root),
`BTreeIndex::insert` allocates a brand new page, builds it as a
one-entry internal node with `tail` = the `PageId` of whatever "left"
ended up being at the top of the propagation chain (the original root's
own page, unless propagation passed through one or more internal splits
first, in which case it's the last one's original page) and a single
entry `(pushed_key, right_page_id)`, and updates
`BTreeIndex::root_page_id` to the new page. This is the same
`(tail, [entry])` shape every other internal split produces - a root
split is not a special case in the code, only in the fact that there was
no existing parent to fold the new entry into.

### Why every split write is safe under ARIES without a compound record

A split touches two or three pages (the original/left, the freshly
allocated right, and - on propagation - the parent, possibly repeated up
to a new root). Each page's rewrite goes through `Node::rebuild`, which
replaces that one page's entire body (everything from byte 12 to the end
of the page) in a single `PageGuard::write` call - one `Update` log
record per page touched, with the whole old body as `before` and the
whole new body as `after`. Nothing wraps these in a single "split"
record: ARIES redo replays each `Update` independently and in order
(idempotently, since redo can replay a change already on disk), and undo
reverses each one by byte range, so a crash mid-split leaves whichever
prefix of these independent writes already reached the log durable, and
recovery reconstructs a consistent state without needing to know they
were "part of a split" at all. `crates/storage/tests/btree_crash_injection.rs`
exercises exactly this: a workload big enough to force a root split,
swept at every write point under every `block_device::DurabilityModel`.

`Node::rebuild` is also why a split never turns into "a loop of small
writes": whether a node ends up with 2 entries or 200, writing its new
body is always exactly one `PageGuard::write` call, because the entries
are assembled into one in-memory buffer first and written as a whole.
`Node::insert_at`'s slot-array shift (the common, non-split path) follows
the same rule at smaller scale - the whole shifted sub-array moves in one
write, never one write per slot.

### Known limitation: a single key duplicated far beyond one leaf's capacity

`nearest_clean_boundary` keeps a *leaf* split from ever separating two
entries that share a key. It does not fully rule out two *internal*
entries in the same node ending up with equal keys: if one external key
value is duplicated enough times to overflow a leaf on its own (not just
span a single split, but force a second, third, ... split of leaves that
are *entirely* copies of that one key), each such split pushes the same
key up as a separator again, and nothing currently coalesces those
repeated pushes into one entry. `tests/btree.rs`'s duplicate-key test
stays well under this threshold (its overflowing leaf mixes duplicates
with distinct neighboring keys, matching the realistic "duplicates span a
split" scenario `check_invariants` is written to accept); reaching the
pathological case would need a single key repeated on the order of a
leaf's whole capacity. Fixing it properly - most likely by ordering
physical leaf entries as `(key, Rid)` rather than `key` alone, so no two
entries are ever byte-identical - is left for whichever future milestone
first needs an index on a low-cardinality column at that scale.

## Key Components

- `MAX_KEY_SIZE` - the largest key `insert` can ever accept: one that,
  entirely alone, would still fit in a freshly created, otherwise-empty
  leaf. Checked up front, before any page is touched, the same way
  `heap::MAX_TUPLE_SIZE` is - see `StorageError::KeyTooLarge`.
- `BTreeIndex<'pool>` - `create(buffer_pool, txn_id)` allocates a root
  page and initializes it as an empty leaf. `open(buffer_pool,
  root_page_id)` opens an existing index whose root is already known - a
  `catalog` entry (`catalog::catalog.MD`'s `Catalog::index_root_page`),
  now that M9.3 wires indexes into table metadata; cheap enough (no I/O,
  just a struct literal) to reconstruct fresh per statement rather than
  cached across one. `root_page_id(&self) -> PageId` returns the current
  root - which `insert` can change, on a root split; the caller is
  responsible for noticing and persisting that change (see
  `catalog::catalog.MD`'s "root_page_id changes on every split" note).
  `get(&self, key) -> Vec<Rid>` descends to the matching leaf and collects
  every entry with that exact key, following the leaf sibling chain if a
  run of duplicates spans a split. `range_scan(&self, start, end) ->
  BTreeRangeIterator` returns an iterator over entries with keys in
  `[start, end)` (`None` on either end means unbounded); construction
  never touches a page, so a bad `start` key surfaces as the iterator's
  first `Err` rather than a panic or an early return from `range_scan`
  itself. `insert(&mut self, txn_id, key, rid)` is M9.2: see "Splitting"
  above. `delete(&mut self, key)` is not implemented yet -
  `todo!("descend to the target leaf, remove the entry, rebalance on
  underflow")`. `leaf_for_start(&self, start) -> Result<(PageId, u16),
  StorageError>` and the associated function `scan_leaf(buffer_pool,
  leaf_page_id, from_slot) -> Result<LeafScan, StorageError>` are the
  primitives `BTreeRangeIterator` is itself built on (below) and exist as
  their own public API for `executor::IndexScanExecutor`
  (`executor::operators::index_scan.MD`), which cannot hold a
  `BTreeRangeIterator` (or any `BTreeIndex`) across its `Executor::next`
  calls - the `Executor` trait reborrows its `ExecutorContext` fresh every
  call, so nothing borrowed from it may live in executor struct fields.
  `leaf_for_start` does the one-time, `&self`-borrowing work of locating a
  starting leaf and slot (cheap to call once, in `IndexScanExecutor::init`,
  then drop); `scan_leaf` is deliberately an associated function, not a
  method, taking `buffer_pool: &BufferPool` directly rather than `&self` -
  exactly `heap::TableHeap::scan_page`'s shape - so a caller can drive it
  from owned `(PageId, u16)` cursor state across repeated calls without
  ever holding a `BTreeIndex` alive between them. `check_invariants(&self,
  key_type: Option<types::DataType>) -> Result<(), String>` walks the
  whole tree from the root and checks, in order: every leaf is
  at the same depth; keys within a node are strictly increasing bytewise
  (ties allowed only in leaves); every key is within the `(low, high)`
  bound inherited from its ancestors' separators, checked against that
  inherited bound rather than only against its immediate parent, so a
  subtree grafted at the wrong place is still caught even if it looks
  locally sorted; no page is reachable from two parents; the leaf
  sibling chain (followed from the leftmost leaf via `tail`) visits
  exactly the same leaves, in the same order, as a left-to-right tree
  walk does; and each node's `data_used` matches the sum of its own entry
  payload lengths, with no entry overlapping the slot array.
- `LeafScan` - the result of `scan_leaf`: `Entry { slot, key, rid }` for a
  present slot, or `EndOfLeaf { next_leaf_page_id }` once `from_slot`
  reaches the leaf's slot count - mirroring `heap::PageScan` exactly, down
  to the name of its "keep going" field.
- `BTreeRangeIterator<'a, 'pool>` - the `range_scan` iterator. Lazily
  resolves its starting leaf and slot on the first `next()` call rather
  than in `range_scan` itself (`RangeState::NotStarted`, via
  `leaf_for_start`), then drives `scan_leaf` one entry at a time
  (`RangeState::InLeaf`), crossing into the next leaf on `EndOfLeaf` until
  either the `end` bound is reached or the sibling chain runs out
  (`RangeState::Done`). Everything this type does is now a thin wrapper
  over `leaf_for_start`/`scan_leaf` - kept as its own type rather than
  folded away because its `Iterator` contract (yielding `(Vec<u8>, Rid)`
  pairs directly) is the more convenient shape for a caller like `get`'s
  sibling-chain walk that *can* hold a live `BTreeIndex` borrow for the
  duration of its use, unlike `executor::IndexScanExecutor`.
- `Node<'a, 'pool>` - private. Wraps a `&mut PageGuard` plus the `TxnId`
  writes should be logged under, and is where every mutating operation on
  a single node's bytes lives: `init` (used by `create`), `will_fit`,
  `find_insert_index`, `insert_at` (the surgical non-split path), and
  `rebuild` (the split/root-creation path, see "Splitting" above).
- `NodeType` - private. `Leaf` or `Internal`, read from/written to the
  1-byte node-type header field.
- `node_type`, `slot_count`, `data_used`, `data_start`, `tail_raw`,
  `checked_slot_count`, `slot_offset`, `slot_entry`, `entry_payload`,
  `entry_key`, `leaf_rid`, `internal_child` - private. Read-only accessors
  over raw page bytes, in the same style as `heap.rs`'s `slotted_*`
  functions - `checked_slot_count` and `entry_payload` report
  `StorageError::CorruptPage` rather than panicking or silently misreading
  a slot count or entry range that doesn't fit the page.
- `build_leaf_payload`, `build_internal_payload` - private. Encode a
  `(key, Rid)` or `(key, PageId)` pair into the entry payload bytes
  described in "Node layout" above.
- `upper_bound`, `lower_bound` - private. Binary search over a node's
  entries by key, operating on page bytes directly (used during descent
  and by the surgical insert path, where re-fetching a `Vec<Vec<u8>>`
  snapshot would be wasted work). `upper_bound` returns the first slot
  whose key is strictly greater than the target - the sorted insertion
  point that places a new entry after any existing ties, and (via
  `child_for_key`) the child a descent should follow. `lower_bound`
  returns the first slot whose key is greater than or equal to the
  target - where `get`/`range_scan` should start scanning for a match.
- `upper_bound_in` - private. The same "first index with a strictly
  greater key" search as `upper_bound`, but over an in-memory
  `&[Vec<u8>]` snapshot (`[u8]::partition_point`) rather than page bytes -
  used once a node has already been read into memory for a split, so the
  split path never re-derives an insertion point from the page it just
  replaced.
- `child_for_key` - private. The child a descent should follow for a
  given key: the right-child pointer of the last entry whose key is
  `<= key`, or `tail` if no entry qualifies (the key is below every
  separator here). See "Node layout" above for why entries pair a key
  with their *right* child rather than their left.
- `byte_balanced_split_point`, `nearest_clean_boundary`,
  `leaf_split_point`, `internal_split_point` - private. See "Choosing the
  split point" above.

## Usage Example

```rust
use common::TxnId;
use storage::btree::BTreeIndex;

let mut index = BTreeIndex::create(&pool, TxnId(1))?;
index.insert(TxnId(1), &encoded_key, rid)?;
assert_eq!(index.get(&encoded_key)?, vec![rid]);

for entry in index.range_scan(None, None) {
    let (key, rid) = entry?;
}
debug_assert!(index.check_invariants(None).is_ok());
```

`catalog::Catalog::create_index`/`index_root_page` (`catalog::catalog.MD`)
are what actually construct a `BTreeIndex` in this workspace -
`Catalog::create_index` calls `create` once, at `CREATE INDEX` time;
every later access opens a fresh handle from the root page id `catalog`
looks up, since `open` is free of I/O. `executor::InsertExecutor`
(`executor::operators::insert.MD`) calls `insert` to maintain every index
on a table as rows are added, turning each indexed column's
`types::Value` into an encoded key via `types::MemcomparableEncode` first;
`executor::IndexScanExecutor` (`executor::operators::index_scan.MD`)
drives `leaf_for_start`/`scan_leaf` to answer a query through the index
instead of a full `heap::TableHeap` scan, chosen by
`planner::optimizer::IndexScanRule`. This module's own tests
(`tests/btree.rs`, `tests/btree_crash_injection.rs`) continue to exercise
the tree directly against a real `BufferPool`, the same way
`tests/table_heap.rs` drives `heap::TableHeap`, independent of that
higher-layer wiring.
