# btree

Integration tests driving `storage::btree::BTreeIndex` directly against a
real `BufferPool`, in the same style `tests/table_heap.rs` uses for
`heap::TableHeap`: no `catalog`/`executor` involved, since no caller in
this workspace builds a `BTreeIndex` yet (see `btree.MD`'s module doc).
Covers both the read path (M9.1) and `insert` with leaf/internal splits
(M9.2).

## Key Components

- `open_pool(dir, pool_size) -> Result<BufferPool, Box<dyn Error>>` -
  the same tempfile-backed `DiskManager`/`DoubleWriteBuffer`/`LogManager`/
  `BufferPool` construction `tests/table_heap.rs` and `tests/recovery.rs`
  use.
- `key_of(i: i32) -> Vec<u8>` - encodes a plain `i32` the same way
  `types::MemcomparableEncode`'s `Value::Integer` case does (sign bit
  flipped, big-endian), so ascending/descending `i32` sequences sort the
  way `BTreeIndex` expects without pulling in a `types` dev-dependency
  just for test keys.
- `a_single_insert_is_found_by_get` - the minimum end-to-end check: insert
  one key, find it, confirm a different key is absent, and that
  `check_invariants` accepts a single-leaf tree.
- `ascending_insert_of_ten_thousand_keys_keeps_invariants_and_every_key_findable`,
  `descending_insert_of_ten_thousand_keys_keeps_invariants_and_every_key_findable` -
  the two split-point stress cases called out in `btree.MD`'s "Choosing
  the split point": both insertion orders repeatedly overflow the
  rightmost (or leftmost) leaf, which is exactly the shape that would
  degenerate into a chain of near-empty leaves under a naive split rule.
  Both check invariants once at the end (checking after every insert
  would make 10,000 inserts prohibitively slow) and then look up every
  key individually.
- `variable_length_varchar_keys_still_split_correctly` - 500 keys with
  lengths cycling from 1 to 500 bytes, so splits happen with a wide mix
  of entry sizes rather than the uniform 4-byte payloads the `i32` tests
  use.
- `a_key_too_large_for_an_empty_node_errors_cleanly` - a key one byte over
  `btree::MAX_KEY_SIZE` is rejected with `StorageError::KeyTooLarge`
  before any page is touched, rather than looping or panicking while
  trying to split a node that could never fit it.
- `root_height_grows_and_root_page_id_changes_on_a_root_split` - inserts
  until `BTreeIndex::root_page_id()` changes from its initial value,
  proving a root split actually replaces the root rather than, say,
  silently discarding the propagated entry.
- `duplicate_keys_spanning_a_split_are_all_returned_by_get` - densely
  fills the tree first so the leaf holding one particular key already has
  other, distinct neighboring entries, then inserts enough duplicates of
  that one key to overflow that specific leaf. The overflowing leaf holds
  a mix of duplicate and non-duplicate entries, so the resulting split
  lands the duplicate run's entries on both sides of the new leaf
  boundary - the realistic version of the scenario `btree.MD`'s "Known
  limitation" section contrasts with the pathological one (a single key
  repeated far beyond one leaf's capacity on its own), which this test
  deliberately stays under.
- `seeded_random_permutation_of_two_thousand_keys_keeps_invariants_and_finds_every_key` -
  shuffles `0..2000` with a fixed-seed `proptest::test_runner::TestRng`
  (reproducible across runs, unlike ambient entropy), checks
  `check_invariants` every 100 inserts rather than only at the end so a
  regression is caught close to its cause, then looks up every key.
- `range_scan_yields_keys_in_order_across_leaf_boundaries` - inserts 3,000
  keys (guaranteeing at least one split), then asserts an unbounded
  `range_scan` yields every key in strictly ascending order and a bounded
  `range_scan(Some(low), Some(high))` yields exactly the keys in
  `[low, high)`.

## Usage Example

```rust
let dir = tempfile::tempdir()?;
let pool = open_pool(dir.path(), 64)?;
let mut index = BTreeIndex::create(&pool, TXN)?;

for i in 0..10_000i32 {
    index.insert(TXN, &key_of(i), Rid::new(PageId(1), 0))?;
}
index.check_invariants().map_err(|e| -> Box<dyn Error> { e.into() })?;
```

Run as part of `cargo test --workspace`, or just this file with
`cargo test -p storage --test btree`. `tests/btree_crash_injection.rs` is
the companion suite covering durability across a crash, rather than
in-memory correctness.
