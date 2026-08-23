# btree

A disk-resident B+tree index: internal nodes hold separator keys and
child `PageId`s, leaf nodes hold keys and the `Rid`s of the matching heap
tuples, and leaves are linked for fast ordered range scans. Keys are
stored as their already-encoded `types::Encode` bytes so the index stays
agnostic to which column(s) it indexes. Currently a stub: every method
that would need real tree traversal is `todo!()`, present to establish
the shape callers above `storage` will code against before the
implementation lands.

## Key Components

- `BTreeIndex<'pool>` - `create(buffer_pool)` creates a brand-new, empty
  index (a single empty leaf as root) - `todo!("allocate a root page,
  initialize it as an empty leaf node")`. `open(buffer_pool,
  root_page_id)` opens an existing index whose root is `root_page_id`.
  `insert(&mut self, key, rid)` inserts `key` mapping to `rid`, splitting
  nodes on overflow and growing the tree's height if the root splits -
  `todo!("descend to the target leaf, insert the entry, split and
  propagate on overflow")`. `delete(&mut self, key)` removes the entry
  for `key`, merging or redistributing underflowing nodes - `todo!("descend
  to the target leaf, remove the entry, rebalance on underflow")`.
  `get(&mut self, key) -> Result<Vec<Rid>, StorageError>` looks up the
  `Rid`s stored under `key` (a B+tree index is not required to enforce
  uniqueness) - `todo!("descend to the target leaf and collect matching
  entries")`. `range_scan(&mut self, start, end) -> BTreeRangeIterator<'_, 'pool>`
  returns an iterator over entries with keys in `[start, end)`, following
  leaf sibling pointers; `start: None` means unbounded below, `end: None`
  means unbounded above.
- `BTreeRangeIterator<'a, 'pool>` - walks matching leaf entries of a
  `BTreeIndex` in key order, crossing leaf pages via their sibling
  pointers. Backs the executor's index-scan operator (once implemented).
  Its `Iterator::next` is `todo!("yield the current leaf entry, then
  advance the cursor or leaf sibling")`.

## Usage Example

```rust
use storage::btree::BTreeIndex;

let mut index = BTreeIndex::create(&mut pool)?; // panics today: todo!()
index.insert(&encoded_key, rid)?;
for entry in index.range_scan(None, None) {
    let (key, rid) = entry?;
}
```

No caller in this workspace constructs a `BTreeIndex` yet - `catalog` and
`executor` both currently route every table through `heap::TableHeap`
only. This module exists so the eventual index-scan planner/executor work
has a fixed, agreed-on shape to code against.
