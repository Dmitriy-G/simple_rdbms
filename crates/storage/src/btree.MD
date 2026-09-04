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
is still `todo!()`, tracked as M15 in `docs/ROADMAP.md`. Its design is
already decided even though it isn't built yet - see "Deleting" below and
`docs/adr/0012-btree-delete-does-not-merge.md`.

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

### Latch crabbing during descent

`BTreeIndex::descend_to_leaf` holds a parent node's `PageReadGuard` until
the child's own `PageReadGuard` has been acquired, releasing the parent
only then - one extra guard's worth of overlap per level, not a
release-then-fetch gap. Without this, a reader could compute `child_for_key`
from a parent's separators, drop the parent's latch, and then have a
concurrent `insert` split that exact parent - and any of its own
ancestors - before the reader's `fetch_page_read(child)` runs, moving the
key it is searching for out of the subtree the reader already committed
to descending into, with no path left to reach it. Crabbing closes this:
a writer's own descent needs an exclusive `fetch_page` on that same
parent to propagate a split into it, and that call blocks for as long as
the reader's shared guard on the parent is held, so the reader's snapshot
of "which child holds this key" can never go stale before the reader has
also latched that child. This follows CLAUDE.md's latch-ordering
invariant - the buffer pool's index lock is never involved here, only two
frames' `RwLock`s held briefly together - and is exercised by
`tests/btree.rs`'s `concurrent_readers_see_a_consistent_view_while_a_writer_forces_repeated_splits`,
several reader threads calling `get`/`range_scan` against a tree one
writer thread is repeatedly splitting. `get`, `range_scan`, and
`leaf_for_start` all call `descend_to_leaf` and inherit this safety for
locating their starting leaf; crossing a leaf's sibling `tail` pointer
once already at leaf level is a separate concern from crabbing during
descent. `get` latch-couples that crossing too: it fetches the next
leaf's `PageReadGuard` before dropping the current leaf's, left-to-right
- the same order `split_leaf` writes the two siblings in, so this
introduces no new deadlock edge - so a concurrent split during the
crossing can no longer make `get` re-read entries it already collected
into `results`. `range_scan`/`scan_leaf`'s own crossing
(`BTreeRangeIterator`, `executor::IndexScanExecutor`) is still a
separate, uncoupled fetch: their caller yields a row back between one
leaf-crossing call and the next, so no latch can be held across that
gap, and latch-coupling cannot close it the way it closes `get`'s.
Instead, `scan_leaf` resumes by comparing against the last entry's own
sort key rather than trusting a remembered slot - see `scan_leaf`'s
"Key Components" entry below for why that closes the same class of bug
without needing a latch held across the gap at all.

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

### Ordering: leaf entries are sorted by `(key, rid)`, not `key` alone

Every leaf entry's *sort key* is `leaf_sort_key(payload)`: the bare key
bytes with the entry's own 6-byte `Rid` trailer appended, taken as one
contiguous slice (the leaf payload is already `key_len | key | page_id |
slot`, so `key` and `rid` sit next to each other with nothing to strip
out). Leaf entries are kept in strictly increasing order by this sort
key, never by the bare key alone, which is what makes every key -
including a key repeated far more times than one leaf can hold - unique
in sort-key terms: two entries can never tie, so a split can land at
*any* boundary without ever separating two byte-identical entries.
Internal nodes are unaffected by this - their own comparison key
(`entry_key`) already holds whatever bytes were pushed up to them as a
separator, and a leaf-split separator is now itself a sort key (see
"Leaf split" below), so `entry_key` on an internal payload already reads
as a sort key with no separate concept needed.

A probe key with no `rid` suffix (an ordinary lookup or a bare key
argument during descent) is always a byte-wise proper prefix of any
separator that shares its key part, and a proper prefix always compares
less than the longer string it prefixes. That means a bare probe key `k`
never equals a separator `k ++ rid` and always routes to the child
*before* that separator - the leftmost child that could hold `k`. This
is deliberate, not a rounding error: with duplicates now able to span a
split at any point (not just a "clean" boundary), a run of entries
sharing `k` can begin partway through the left child too, so landing on
the left child and then following the leaf sibling chain (`get`) or the
range-scan cursor (`BTreeRangeIterator`) forward is the only way to be
sure every instance of `k` is found regardless of which physical leaf it
ended up in. `BTreeIndex::insert`, by contrast, must not use this
leftmost-routing behavior for its own descent: a new entry has to land
in whichever leaf its *own* sort key (`key ++ rid`, not the bare key)
actually belongs in, which may be a leaf several splits to the right of
the very first one that ever held `key` - so `insert` descends with
`leaf_sort_key(&leaf_payload)`, not the bare key, precisely so a new
duplicate finds the current tail of its own run instead of always
piling into the oldest leaf that ever held that key.

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
exercise this). `leaf_split_point` is exactly this index, nudged one
slot to the right and clamped to leave at least one entry on each side
(`byte_balanced_split_point(entries).saturating_add(1).clamp(1, entries.len()
- 1)`) - no further adjustment is needed, because sort-key uniqueness
(above) means every boundary is already a legal one to split on.
`internal_split_point` needs its own, separate clamp (`entries.len() -
2`, not `- 1`) because an internal split additionally *removes* the
pushed entry from both halves - see "Internal split" below - but no
duplicate-avoidance logic of its own, for the same reason.

### Leaf split

Given the merged `n + 1` entries and a split index `mid` (any index now
works - see "Ordering" above):

- **Left** (the original page, unchanged `PageId`) keeps entries
  `[0, mid)`.
- **Right** (a freshly allocated page) keeps entries `[mid, n + 1)`.
- `left.tail` becomes the right page's new `PageId`.
- `right.tail` becomes whatever `left.tail` was *before* the split (the
  original next-sibling pointer) - the right leaf inherits the left
  leaf's old place in the sibling chain.
- The separator copied up to the parent is `leaf_sort_key` of `right`'s
  first entry - the bare key *plus* that entry's `Rid` trailer, not just
  the bare key. The entry itself **stays in the leaf** - unlike the
  internal case below, nothing is removed, because leaves must hold
  every key for `get`/`range_scan` to find it without consulting internal
  nodes at all; only a copy of its sort key travels up.

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

### The `rebuild` capacity guard

Every caller of `Node::rebuild` computes a split point *before* calling
it and is trusted to have gotten the arithmetic right - `rebuild` used to
write the slot array and entry bytes it was handed with no check that
they actually fit the page, so a wrong split point silently overwrote
entry data with the slot array instead of failing loudly. `rebuild` now
computes `required = local_header_size + entries.len() * SLOT_SIZE +
entries.iter().map(Vec::len).sum()` before writing anything and returns
`StorageError::NodeOverflow { page_id, required, capacity }` instead of
touching the page if `required` exceeds the body's actual capacity. This
is a safety net, not a substitute for the split arithmetic being correct
- it turns a bug in `leaf_split_point`/`internal_split_point` (or a
future change to either) into an immediate, loud error instead of a
silently corrupted page, the same way `checked_slot_count` turns a bad
slot count into `CorruptPage` instead of an out-of-bounds read. Before
the `(key, rid)` leaf ordering above existed, a leaf entirely full of one
repeated key had no clean boundary to split on, `leaf_split_point` fell
back to index `0`, and `rebuild` wrote every entry onto the "right" node
- the exact silent corruption this guard now converts into a loud
`NodeOverflow` instead. Now that leaf entries sort by `(key, rid)`, no
legitimate split can ever produce a `required` larger than the page, so
this guard should never fire in practice; it stays as the same kind of
defense in depth `checked_slot_count` already is.

## Deleting

Not implemented yet (`todo!()`, tracked as M15), but the design is
already settled: `docs/adr/0012-btree-delete-does-not-merge.md`. Delete
removes exactly the entry located by its full sort key (`key ++ rid`, the
same lookup a duplicate-tolerant `insert` places by - see "Ordering"
above), the same way `insert` already descends with `leaf_sort_key`
rather than the bare key. Two cases: if the leaf still holds at least one
entry afterward, nothing else happens - a node at any occupancy, even
one entry, is left as-is, deliberately, not as a gap to fill in later. If
the leaf becomes completely empty, it is unlinked from the leaf sibling
chain and removed from its parent's entries; if that leaves the parent a
degenerate one-child internal node (nothing but its own `tail`), the
parent is spliced out the same way, one level at a time, up through a
possible root collapse. No sibling is ever inspected for a borrow, and no
two non-empty nodes are ever merged - the same choice Postgres's `nbtree`
makes, and the only one that makes sense before M24 gives this engine a
free list a merged-away page could actually be returned to; until then,
an unlinked leaf's page is simply orphaned (unreachable, not reclaimed),
the same as every other page nothing in this codebase frees yet.

## Key Components

- `MAX_KEY_SIZE` - the largest key `insert` can ever accept: one that,
  entirely alone, would still fit in a freshly created, otherwise-empty
  leaf, *and* would still fit as a lone internal separator once that same
  `(key, rid)` pair is later pushed up by a split - the formula subtracts
  `CHILD_TRAILER_SIZE` on top of the leaf-only budget for exactly that
  reason, since a separator carries the `Rid` suffix a leaf key has
  *plus* a child pointer a leaf entry does not. Checked up front, before
  any page is touched, the same way `heap::MAX_TUPLE_SIZE` is - see
  `StorageError::KeyTooLarge`.
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
  above. `delete(&mut self, key)` is not implemented yet - `todo!()`, its
  message still describing a textbook merge-and-rebalance delete that
  "Deleting" above and ADR 0012 have since superseded with a no-merge
  design; the signature itself will also gain an `rid: Rid` parameter
  once M15 implements it, to locate the exact duplicate entry by its full
  sort key rather than removing an arbitrary one sharing `key`.
  `leaf_for_start(&self, start) -> Result<PageId, StorageError>` and the
  associated function `scan_leaf(buffer_pool, leaf_page_id, after) ->
  Result<LeafScan, StorageError>` are the primitives `BTreeRangeIterator`
  is itself built on (below) and exist as their own public API for
  `executor::IndexScanExecutor` (`executor::operators::index_scan.MD`),
  which cannot hold a `BTreeRangeIterator` (or any `BTreeIndex`) across
  its `Executor::next` calls - the `Executor` trait reborrows its
  `ExecutorContext` fresh every call, so nothing borrowed from it may
  live in executor struct fields. `leaf_for_start` does the one-time,
  `&self`-borrowing work of locating a starting leaf (cheap to call once,
  in `IndexScanExecutor::init`, then drop) - just the leaf, no slot: it
  used to also compute a starting slot via `lower_bound_leaf`, but that
  position is now `scan_leaf`'s own job, computed fresh from `after` on
  every call rather than trusted from an earlier snapshot. `scan_leaf` is
  deliberately an associated function, not a method, taking
  `buffer_pool: &BufferPool` directly rather than `&self` - exactly
  `heap::TableHeap::scan_page`'s shape - so a caller can drive it from
  owned `(PageId, Option<Vec<u8>>)` cursor state across repeated calls
  without ever holding a `BTreeIndex` alive between them. `after` is
  `None` for the very first call on a fresh, unbounded scan, or the bare
  probe key for a bounded `start` (`BTreeRangeIterator`/
  `IndexScanExecutor` pass their own `start` straight through as the
  first call's `after`, which is why `leaf_for_start` no longer needs to
  compute a position itself); every call after the first passes the full
  `leaf_sort_key` of the last entry the caller actually yielded.
  `scan_leaf` finds the first entry whose `leaf_sort_key` is *strictly
  greater* than `after` (`lower_bound_leaf`, skipping one slot forward on
  an exact match - which only happens when `after` is itself a full sort
  key from a previous entry, never for a bare probe key, since a bare key
  can never equal a stored sort key - see "Ordering" below), rather than
  trusting a raw slot index a concurrent split could have invalidated.
  This is what makes crossing from one leaf to the next safe even though
  no latch is held across the caller's yield (`BTreeRangeIterator::next`
  returns between leaves, and `IndexScanExecutor::next` returns between
  every row): wherever the next matching entry physically ends up - the
  same leaf, a leaf it split into, or a leaf several splits further along
  - comparing against the last entry's own sort key finds it exactly
  once, never by re-trusting a position that may no longer mean what it
  meant when it was recorded. Latch-coupling (the fix `get`'s sibling
  crossing uses, see "Latch crabbing during descent" above) cannot do
  this job here, because it only helps a caller that never lets go of a
  guard across the race window - `BTreeRangeIterator`/`IndexScanExecutor`
  always do, by design: `executor::IndexScanExecutor` cannot hold a page
  guard across a yielded row at all, since the `Executor` trait reborrows
  its `ExecutorContext` fresh on every call (`executor::operators::index_scan.MD`),
  and `BTreeRangeIterator` gives up its guard between `next()` calls for
  the same reason `scan_leaf` is a free function rather than a method -
  so neither can be "the caller that never lets go" latch-coupling
  needs. `check_invariants(&self,
  key_type: Option<types::DataType>) -> Result<(), String>` walks the
  whole tree from the root and checks, in order: every leaf is
  at the same depth; keys within a node are strictly increasing bytewise -
  compared as `leaf_sort_key` for a leaf's own entries and as `entry_key`
  for an internal node's separators, so leaf ties are never allowed
  either, matching the ordering invariant above; every key is within the
  `(low, high)`
  bound inherited from its ancestors' separators, checked against that
  inherited bound rather than only against its immediate parent, so a
  subtree grafted at the wrong place is still caught even if it looks
  locally sorted; no page is reachable from two parents; the leaf
  sibling chain (followed from the leftmost leaf via `tail`) visits
  exactly the same leaves, in the same order, as a left-to-right tree
  walk does; and each node's `data_used` matches the sum of its own entry
  payload lengths, with no entry overlapping the slot array.
- `LeafScan` - the result of `scan_leaf`: `Entry { slot, key, sort_key,
  rid }` for a present slot, or `EndOfLeaf { next_leaf_page_id }` once no
  entry past `after` remains on the leaf - mirroring `heap::PageScan`
  exactly, down to the name of its "keep going" field. `sort_key` is the
  entry's full `leaf_sort_key` (key bytes plus the `Rid` trailer,
  precomputed here so a caller resuming the scan never has to rebuild
  that encoding itself); `slot` is the position `scan_leaf` actually
  found the entry at on this call, informational only - a caller must
  never carry it into a *later* call the way it used to, which is exactly
  the bug this shape replaces.
- `BTreeRangeIterator<'a, 'pool>` - the `range_scan` iterator. Lazily
  resolves its starting leaf on the first `next()` call rather than in
  `range_scan` itself (`RangeState::NotStarted`, via `leaf_for_start`),
  then drives `scan_leaf` one entry at a time (`RangeState::InLeaf { page_id,
  after }`), crossing into the next leaf on `EndOfLeaf` until either the
  `end` bound is reached or the sibling chain runs out (`RangeState::Done`).
  `after` starts as `NotStarted`'s own `start` bound and, from the first
  yielded entry onward, is always that entry's `sort_key` - carried
  unchanged across an `EndOfLeaf` crossing into the next leaf, precisely
  so a leaf that split between two `next()` calls can never cause an
  already-yielded entry to be re-yielded from whichever physical page it
  ended up on (see `scan_leaf`'s entry above). Everything this type does
  is now a thin wrapper over `leaf_for_start`/`scan_leaf` - kept as its
  own type rather than folded away because its `Iterator` contract
  (yielding `(Vec<u8>, Rid)` pairs directly) is the more convenient shape
  for a caller like `get`'s sibling-chain walk that *can* hold a live
  `BTreeIndex` borrow for the duration of its use, unlike
  `executor::IndexScanExecutor`.
- `Node<'a, 'pool>` - private. Wraps a `&mut PageGuard` plus the `TxnId`
  writes should be logged under, and is where every mutating operation on
  a single node's bytes lives: `init` (used by `create`), `will_fit`,
  `find_insert_index` (dispatches to `upper_bound_leaf` or `upper_bound`
  depending on `node_type`, so a leaf compares by sort key and an
  internal node by its separator's own stored bytes), `insert_at` (the
  surgical non-split path), and `rebuild` (the split/root-creation path,
  see "Splitting" above - and "The `rebuild` capacity guard" for its
  `NodeOverflow` precondition).
- `NodeType` - private. `Leaf` or `Internal`, read from/written to the
  1-byte node-type header field.
- `node_type`, `slot_count`, `data_used`, `data_start`, `tail_raw`,
  `checked_slot_count`, `slot_offset`, `slot_entry`, `entry_payload`,
  `entry_key`, `leaf_sort_key`, `leaf_rid`, `internal_child` - private.
  Read-only accessors over raw page bytes, in the same style as
  `heap.rs`'s `slotted_*` functions - `checked_slot_count` and
  `entry_payload` report `StorageError::CorruptPage` rather than
  panicking or silently misreading a slot count or entry range that
  doesn't fit the page. `leaf_sort_key` is `entry_key` extended to also
  include the entry's `Rid` trailer - see "Ordering" above.
- `build_leaf_payload`, `build_internal_payload` - private. Encode a
  `(key, Rid)` or `(key, PageId)` pair into the entry payload bytes
  described in "Node layout" above.
- `upper_bound`, `upper_bound_leaf`, `lower_bound_leaf` - private. Binary
  search over a node's entries, operating on page bytes directly (used
  during descent and by the surgical insert path, where re-fetching a
  `Vec<Vec<u8>>` snapshot would be wasted work). `upper_bound` compares
  by `entry_key` and is the internal-node search: it returns the first
  slot whose separator is strictly greater than the target, i.e. (via
  `child_for_key`) the child a descent should follow. `upper_bound_leaf`
  is the same "first index with a strictly greater key" search but for a
  leaf, comparing by `leaf_sort_key` against a caller-supplied sort key,
  so ties can never stop the search early even under heavy duplication.
  `lower_bound_leaf` returns the first leaf slot whose sort key is
  greater than or equal to the target - where `get`/`range_scan` should
  start scanning for a match; called with a bare key (no `rid` suffix),
  it lands on the first of any run of duplicates, which is what both
  callers want (see "Ordering" above).
- `upper_bound_in`, `upper_bound_in_leaf` - private. The same two
  searches as `upper_bound`/`upper_bound_leaf`, but over an in-memory
  `&[Vec<u8>]` snapshot (`[u8]::partition_point`) rather than page bytes -
  used once a node has already been read into memory for a split, so the
  split path never re-derives an insertion point from the page it just
  replaced.
- `child_for_key` - private. The child a descent should follow for a
  given key: the right-child pointer of the last entry whose key is
  `<= key`, or `tail` if no entry qualifies (the key is below every
  separator here). See "Node layout" above for why entries pair a key
  with their *right* child rather than their left, and "Ordering" above
  for why a bare key always routes left of any separator sharing its key
  part.
- `byte_balanced_split_point`, `leaf_split_point`, `internal_split_point`
  - private. See "Choosing the split point" above.

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
