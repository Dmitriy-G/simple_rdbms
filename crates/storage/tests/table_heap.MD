# table_heap

`TableHeap` behavior: inserting enough tuples to span several pages and
reading them all back, a tuple too large for any page producing a clear
error instead of a panic (or an infinite loop), and a from-scratch,
panic-freedom property test against arbitrary page bytes.

## Key Components

- `TXN` - `TxnId(0)`.
- `open_pool(pool_size)` - opens a `BufferPool` of `pool_size` frames over
  a fresh temp-dir database.
- `insert_spans_pages_and_iterates_every_tuple_back` - inserts 40 tuples
  (300 bytes each, forcing multiple page allocations in a 4-frame pool)
  and checks every one reads back via both `get_tuple` and `iter()`.
- `insert_tuple_too_large_for_any_page_errors_cleanly` - a tuple one byte
  larger than `MAX_TUPLE_SIZE` must be rejected with
  `StorageError::TupleTooLarge`, not accepted or panicked on.
- `get_tuple_returns_none_for_deleted_slot` - a deleted tuple's `Rid`
  reads back as `None`.
- `scanning_a_page_whose_init_never_reached_disk_is_a_clean_empty_scan` -
  a page allocated but never `SlottedPage::init`-ed (as happens when a
  process exits before flushing a freshly-created page) stays all-zero.
  `NO_NEXT_PAGE` being `PageId(0)` - the one page id a heap chain can
  never legitimately contain - means that decodes as a valid, empty,
  terminal page rather than corruption: an uninitialized page is usable,
  not merely non-fatal. This test proves the scan side of that.
- `inserting_into_a_never_initialized_page_still_works` - proves the
  write side too: inserting into a heap whose only page was never
  `SlottedPage::init`-ed must succeed, and the tuple must read back.
- `slot_count_above_max_slots_still_reports_corruption` - `MAX_SLOTS`
  remains a backstop against a page whose slot count could never have
  come from `SlottedPage::insert` (a non-heap page misread as one, say) -
  written directly here (byte 12, the start of the slotted-page header,
  since the checksum and `page_lsn` occupy the reserved `0..12` prefix)
  since `SlottedPage::insert` itself can't be driven past it.
- `no_page_content_can_panic_a_reader` - no sequence of page bytes,
  however implausible, may panic a reader. Installs 4096 fully random
  bytes as a heap page's raw content (via a seeded `TestRng` for
  reproducibility) and drives every public entry point that parses page
  bytes through it: iterating the heap (bounded to 10,000 entries, so an
  adversarial `next_page_id` that happens to loop back on itself doesn't
  hang the test instead of panicking - astronomically unlikely from
  random bytes, but the bound keeps that outcome from hanging either
  way), looking up every possible slot by `Rid`, and inserting a tuple.
  This is the property the original all-zero-page bug was really an
  instance of; `proptest`'s own panic-catching (needed to shrink
  counterexamples) is what turns any surviving panic into a normal,
  reproducible test failure here rather than an aborted test binary.

## Usage Example

```rust
let (pool, _dir) = open_pool(4)?;
let mut heap = TableHeap::create(&pool, TXN)?;
let rid = heap.insert_tuple(TXN, b"a tuple's encoded bytes")?;
assert_eq!(heap.get_tuple(rid)?.as_deref(), Some(b"a tuple's encoded bytes".as_slice()));
```

Run via `cargo test -p storage --test table_heap`, driving
`storage::heap::TableHeap` end-to-end - the layer `catalog` and
`executor`'s `SeqScanExecutor`/`InsertExecutor` build on directly.
