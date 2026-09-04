# btree

Integration tests driving `storage::btree::BTreeIndex` directly against a
real `BufferPool`, in the same style `tests/table_heap.rs` uses for
`heap::TableHeap`: no `catalog`/`executor` involved, even though both now
build a `BTreeIndex` in the real query path (see `btree.MD`'s module
doc) - this file stays focused on the tree's own correctness in
isolation, independent of that higher-layer wiring, which
`crates/catalog/tests/`, `crates/executor/tests/`, and
`crates/engine/tests/` cover instead. Covers the read path (M9.1),
`insert` with leaf/internal splits (M9.2), and the `leaf_for_start`/
`scan_leaf` primitives M9.3 added for `executor::IndexScanExecutor`.

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
  boundary.
- `a_leaf_full_of_one_repeated_key_splits_without_corruption` - the
  pathological case the test above deliberately stays under: 1,200
  inserts of one single key, so the *entire* overflowing leaf (and every
  leaf it subsequently splits into) holds nothing but copies of that key.
  Before the `(key, rid)` leaf ordering in `btree.MD`'s "Ordering"
  section, this had no clean boundary to split on at all and corrupted
  the page (see `btree.MD`'s "The `rebuild` capacity guard"); this test
  is what pins the fix down. Checks invariants every 50 inserts, then
  confirms every one of the 1,200 `Rid`s is still returned by `get`.
- `a_long_duplicate_run_between_distinct_keys_stays_ordered` - 200
  distinct keys, then 800 inserts of one hot key, then 200 more distinct
  keys above it: exercises a duplicate run large enough to force several
  splits on its own while it sits between (not at the very start or end
  of) a range of ordinary keys, and confirms `range_scan` still yields
  every entry - hot duplicates included - in non-decreasing key order
  across the whole tree.
- `a_maximum_length_key_survives_a_split` - inserts entries at exactly
  `MAX_KEY_SIZE`, which since the "Ordering" fix must leave room for that
  same `(key, rid)` pair to later become an internal separator (a `Rid`
  suffix plus a child pointer, not just the bare key) without overflowing
  a node - see `MAX_KEY_SIZE`'s entry in `btree.MD`'s "Key Components".
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
- `a_leaf_split_logs_well_under_the_naive_two_full_page_cost` - asserts,
  via `BufferPool::log_bytes_appended`, that a plain (non-root) leaf split
  logs well under the cost of writing both split nodes' full before/after
  images - see `btree.MD`'s "Splitting" for the diff-based rebuild logging
  this pins down.
- `CONCURRENT_BASELINE_KEYS` / `CONCURRENT_WRITER_INSERTS` /
  `CONCURRENT_READER_THREADS` / `CONCURRENT_READER_ITERATIONS` - sizing
  constants for the concurrent descent test below: 1,000 keys inserted
  single-threaded before any concurrency starts, 6,000 more inserted by the
  one writer thread while readers run (enough to force several leaf splits,
  at least one internal split, and at least one root split), 4 reader
  threads, 400 iterations each.
- `concurrent_readers_see_a_consistent_view_while_a_writer_forces_repeated_splits` -
  the latch-crabbing regression test for `btree.MD`'s "Latch crabbing
  during descent": inserts `CONCURRENT_BASELINE_KEYS` keys single-threaded,
  publishes the resulting `root_page_id` into a shared `AtomicU32`, then
  runs `CONCURRENT_READER_THREADS` reader threads concurrently with the
  main thread inserting `CONCURRENT_WRITER_INSERTS` more keys. Each reader
  opens a fresh `BTreeIndex::open` against whatever root the atomic
  currently holds (a valid root from some point in the writer's serial
  insert sequence, never a torn one, since the writer only publishes it
  after `insert` returns and every page write is itself synchronized by
  the buffer pool's per-frame `RwLock`), calls `get` on one baseline key
  per iteration and asserts it returns exactly that key's original `Rid`,
  and every 20th iteration also runs a full `range_scan` and asserts every
  baseline key is present in it with its correct `Rid`, *and* that the
  whole scan comes back strictly ascending by `(key, rid)` end to end -
  no duplicate, no out-of-order pair. That second half used to be a
  weaker check ("every read returns either the pre-insert or post-insert
  value and never an error or a missing key") because P-6 found
  `range_scan` re-delivering an already-yielded entry across a concurrent
  leaf-boundary crossing; `scan_leaf`'s resume-by-sort-key fix
  (`btree.MD`'s `scan_leaf`/`LeafScan` entries) is what makes the
  stronger, duplicate-free assertion hold now. Ends by joining every
  reader (any `Err` fails the test with the exact key and mismatch) and
  running `check_invariants` on the writer's own final tree.
  Timing-dependent like `crates/storage/tests/buffer_pool_concurrency.rs`'s
  races: it exercises the crabbing and resume fixes under real thread
  scheduling rather than deterministically forcing the exact
  interleaving, so a clean run is evidence, not proof - see that file's
  `.MD` for the same caveat.
- `DUP_GET_BASELINE_KEYS` / `DUP_GET_INITIAL_DUPLICATES` /
  `DUP_GET_READER_THREADS` / `DUP_GET_READER_ITERATIONS` /
  `DUP_GET_WRITER_DUPLICATES` - sizing constants for the test below: 200
  distinct keys plus 40 duplicates of one key inserted single-threaded
  (enough to already span a leaf split, matching
  `duplicate_keys_spanning_a_split_are_all_returned_by_get`'s magnitude),
  4 reader threads running 400 iterations each while the main thread
  inserts 3,000 more duplicates of that same key.
- `get_on_a_duplicated_key_never_returns_a_rid_twice_while_its_own_run_keeps_splitting` -
  the regression test for `get`'s own sibling-crossing fix (`btree.MD`'s
  "Latch crabbing during descent"): grows one key's duplicate run
  concurrently with reader threads repeatedly calling `get` on that exact
  key and asserting the returned `Rid`s are never repeated (sorted, then
  `Vec::dedup`'d, and compared by length) - the property latch-coupling
  `get`'s leaf crossing restores. Deliberately targets the key's *own*
  chain rather than a neighboring key's, since growing a neighbor's run
  heavily enough to reliably force splits near this key surfaced a
  separate, deeper bug in `insert`'s multi-level split cascade - filed
  rather than fixed here (P-18). Ends with `check_invariants` on the
  writer's own final tree. Same timing-dependent caveat as the test
  above.
- `scan_leaf_and_leaf_for_start_cross_a_leaf_boundary_like_range_scan_does` -
  drives `leaf_for_start`/`scan_leaf` by hand, the same way
  `executor::IndexScanExecutor` must (owned `(PageId, Option<Vec<u8>>)`
  cursor state, no live `BTreeIndex` held across calls), over the same
  3,000-key tree `range_scan_yields_keys_in_order_across_leaf_boundaries`
  uses: asserts the manually-driven walk crosses at least one
  `LeafScan::EndOfLeaf` boundary, yields the same 3,000 keys in the same
  ascending order `range_scan` does, and that `scan_leaf` called with
  `leaf_for_start(Some(key))`'s leaf and `Some(key)` as `after` lands
  exactly on that key.

## Usage Example

```rust
let dir = tempfile::tempdir()?;
let pool = open_pool(dir.path(), 64)?;
let mut index = BTreeIndex::create(&pool, TXN)?;

for i in 0..10_000i32 {
    index.insert(TXN, &key_of(i), Rid::new(PageId(1), 0))?;
}
index.check_invariants(None).map_err(|e| -> Box<dyn Error> { e.into() })?;
```

Run as part of `cargo test --workspace`, or just this file with
`cargo test -p storage --test btree`. `tests/btree_crash_injection.rs` is
the companion suite covering durability across a crash, rather than
in-memory correctness.
