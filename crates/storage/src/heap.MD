# heap

The on-disk slotted-page heap file format: `SlottedPage`, a view over a
single page's bytes, and `TableHeap`, the singly-linked chain of slotted
pages that stores a table's tuples before (or in place of) any index.

## Key Components

- `NO_NEXT_PAGE` - `PageId(0)`, the sentinel stored in a slotted page's
  `next_page_id` field meaning "this is the last page in the chain." Page
  0 is permanently the database's own header page (see `disk::DiskManager`)
  and so can never legitimately be a heap page, which makes `PageId(0)` an
  unambiguous choice: it's also what a page's `next_page_id` field reads
  as when the page is all zero, so a page whose `init()` write never
  reached disk (e.g. a crash between allocating and initializing it)
  still decodes as a valid, terminal, empty page instead of corrupt. This
  is the load-bearing design decision the rest of this module's
  zero-is-safe behavior (`checked_slot_count`, `slotted_data_start`,
  `SlottedPage::init` being a no-op on an already-zero page) all build on.
- `SLOT_COUNT_RANGE`/`DATA_USED_RANGE`/`NEXT_PAGE_ID_RANGE` - byte layout
  of a slotted page's header, which sits just after the page-wide
  checksum (`page::CHECKSUM_RANGE`, bytes `0..4`) and `page_lsn`
  (`page::PAGE_LSN_RANGE`, bytes `4..12`) prefixes.
- `HEADER_SIZE` - `20`: the checksum (4 bytes) and `page_lsn` (8 bytes)
  prefixes followed by the slotted-page header (`slot_count`,
  `data_used`, `next_page_id`; 8 more bytes) - the offset the slot array
  starts at.
- `SLOT_SIZE` - `4`: each slot is a `(u16 offset, u16 len)` pair.
- `MAX_SLOTS` - `(PAGE_SIZE - HEADER_SIZE) / SLOT_SIZE`, the largest
  number of slots a slot array can hold without running past the page.
  `SlottedPage::insert` never lets `slot_count` grow past this, so a
  stored `slot_count` above it can only come from a page that was never
  actually written as a slotted page. With `NO_NEXT_PAGE` now `PageId(0)`
  (so an all-zero, uninitialized page is a valid empty page rather than
  corrupt) and `slotted_read` validating every tuple range it hands out,
  this is a last-resort guard against misreading some *other* kind of
  page (e.g. the page-0 file header) as a heap page - it should be
  unreachable in practice, not the primary defense it once was.
- `slot_offset(slot) -> usize`/`read_u16(bytes, at) -> u16` - private
  helpers.
- `slotted_slot_count(bytes) -> u16` - private. Reads the number of slots
  (live or tombstoned) allocated on a slotted page's raw bytes.
- `checked_slot_count(bytes, page_id) -> Result<u16, StorageError>` -
  private. Reads and validates the slot count for a page being scanned
  from disk. A `slot_count` above `MAX_SLOTS` cannot come from a real
  slotted page; this is a backstop against misreading some other kind of
  page as a heap page, not the mechanism that makes an ordinary
  uninitialized page safe - that comes from `NO_NEXT_PAGE` being
  `PageId(0)`, a value a zeroed page could never falsely produce.
- `slotted_data_used(bytes) -> u16` - private. Reads the number of
  tuple-data bytes currently used. Tuple bytes are packed backward from
  the end of the page, so this grows from `0` as tuples are inserted,
  rather than storing an absolute offset - which is what makes an
  all-zero page decode as a valid, empty page instead of one with no free
  space at all.
- `slotted_data_start(bytes) -> usize` - private. The offset (from the
  start of the page) at which tuple data currently begins: `PAGE_SIZE`
  minus however many bytes are in use. Saturates at `0` rather than
  underflowing: a stored `data_used` above `PAGE_SIZE` cannot come from a
  real page, only a corrupt one, and reading `0` free space for such a
  page is the same safe "can't insert here" outcome `slotted_free_space`
  already produces for other kinds of corruption.
- `slotted_next_page_id(bytes) -> PageId` - private. Reads the id of the
  next page in this heap's chain, or `NO_NEXT_PAGE`.
- `slotted_slot_entry(bytes, slot, page_id) -> Result<Option<(u16, u16)>, StorageError>` -
  private. Looks up slot `slot`'s `(offset, len)` entry: `Ok(None)` if
  `slot` is past the allocated slot array, `Err(CorruptPage)` if the
  page's own slot count is invalid, checked first so every entry this
  does return is guaranteed to sit within the page.
- `slotted_read(bytes, slot, page_id) -> Result<Option<&[u8]>, StorageError>` -
  private. The raw bytes of the tuple in `slot`. `Ok(None)` if the slot
  doesn't exist or is tombstoned (`len == 0`). `Err(CorruptPage)` if the
  slot's recorded `(offset, len)` doesn't describe a valid tuple - a
  range that runs past the page, or one that overlaps the slot array
  itself - bytes that `SlottedPage::insert` could never have produced.
- `slotted_free_space(bytes) -> usize` - private. The number of free
  bytes available for a new slot plus its tuple data.
- `SlottedPage<'a, 'pool>` - a view over a `PageGuard`'s bytes as a
  slotted page: a header, a slot array growing forward from just after
  the header, and tuple bytes packed backward from the end of the page.
  Slots hold an `(offset, length)` pair so a `Rid`'s slot number stays
  stable even as earlier tuples on the page are deleted or resized, which
  is what lets indexes point at a `Rid` without being invalidated by
  unrelated writes to the same page. Every mutating method goes through
  `PageGuard::write`, so every change to a slotted page's bytes is logged
  to the write-ahead log before it is applied - there is no other way to
  reach a page's bytes from here. `SlottedPage::new(guard, txn_id)` wraps
  `guard` for slotted-page-layout access, logging any mutation on behalf
  of `txn_id`. `data(&self)` (private) returns the guard's raw bytes.
  `init(&mut self)` initializes an empty page's header: no slots, no
  tuple data used yet, and no next page in the chain. Every field's
  "empty" value is `0` (`NO_NEXT_PAGE` is `PageId(0)`), so this is a
  no-op on a page that's already all zero - kept for explicitness, since
  a page that never gets this call (e.g. one whose initializing write was
  lost before reaching disk) is still a valid empty page, not a corrupt
  one. `slot_count(&self) -> u16` is the number of slots currently
  allocated, including any occupied by deleted (tombstoned) tuples.
  `read(&self, slot) -> Result<Option<&[u8]>, StorageError>` reads the
  tuple in `slot`. `free_space(&self) -> usize` is the free bytes
  available for a new slot plus its tuple data. `next_page_id(&self) ->
  Option<PageId>` is the next page in this heap's chain, or `None` if
  this is the chain's last page. `set_next_page_id(&mut self, next)`
  links this page to `next`; `next` must never be page 0 (a
  `debug_assert!` guards this - it's the database's reserved header
  page, and using it as a heap page id would collide with
  `NO_NEXT_PAGE`). `insert(&mut self, data) -> Result<Option<u16>, StorageError>`
  appends `data` as a new tuple, returning its slot number, or `None` if
  the page does not have enough free space. `delete(&mut self, slot)`
  tombstones `slot`, freeing its bytes for future compaction without
  shifting other slots' numbers (`// TODO(M5): vacuum - reclaim
  tombstoned tuple bytes via compaction`).
- `MAX_TUPLE_SIZE` - the largest tuple a single, otherwise-empty page can
  ever hold: header, one slot entry, and the tuple's own bytes must all
  fit in `PAGE_SIZE`. A tuple larger than this can never be inserted, no
  matter how many pages are tried, so `TableHeap::insert_tuple` rejects
  it up front instead of looping forever allocating pages for it.
- `TableHeap<'pool>` - the on-disk heap: an unordered collection of a
  table's tuples, stored as a singly-linked chain of slotted pages
  starting at `first_page_id`. This is the default storage for a table
  before (or in place of) any index. `TableHeap::open(buffer_pool,
  first_page_id)` opens the table heap whose first page is
  `first_page_id`. `TableHeap::create(buffer_pool, txn_id) ->
  Result<Self, StorageError>` creates a brand-new, empty table heap
  (allocates its first page) and returns a handle to it, logging on
  behalf of `txn_id`. `first_page_id(&self) -> PageId` returns this
  heap's first page. `insert_tuple(&mut self, txn_id, tuple_bytes) ->
  Result<Rid, StorageError>` inserts `tuple_bytes`, returning the `Rid`
  it now lives at. Tries the last-known page first, allocating and
  linking a new one if it doesn't fit there. `append_page_after(&mut
  self, txn_id, after) -> Result<PageId, StorageError>` (private)
  allocates a new, empty page and links it after `after` in the chain.
  `get_tuple(&self, rid) -> Result<Option<Vec<u8>>, StorageError>` reads
  the tuple bytes at `rid`, or `None` if that slot holds no live tuple.
  `delete_tuple(&mut self, txn_id, rid)` deletes the tuple at `rid`.
  `iter(&self) -> TableIter<'_, 'pool>` returns an iterator over every
  live tuple in the heap, in physical page/slot order, holding a page
  pinned only while reading that page's tuples out, never across the
  whole scan. `scan_page(buffer_pool, page_id, from_slot) ->
  Result<PageScan, StorageError>` scans `page_id` for the next live
  tuple at or after `from_slot`, fetching and releasing exactly one pin -
  the primitive a caller drives one step at a time to build its own lazy,
  page-at-a-time cursor over a heap (see `executor::SeqScanExecutor`)
  without ever holding a page pinned across its own iteration boundary.
- `PageScan` - the outcome of `TableHeap::scan_page`. `Tuple { slot,
  bytes }` is a live tuple at `slot`, with its raw bytes; the caller
  should resume this same page at `slot + 1` next call. `EndOfPage {
  next_page_id }` means no live tuple remains at or after `from_slot`;
  the caller should move on to `next_page_id` (`None` at the end of the
  chain) and resume there at slot `0`.
- `TableIter<'a, 'pool>` - walks a `TableHeap` page by page, yielding
  `(Rid, Vec<u8>)` pairs for every live tuple. Backs the executor's
  `SeqScan` operator. Each page is fetched once, its live tuples copied
  out, and unpinned before the next page is fetched - no page stays
  pinned across the whole scan.

## Usage Example

```rust
use storage::heap::TableHeap;

let mut heap = TableHeap::create(&pool, txn_id)?;
let rid = heap.insert_tuple(txn_id, &encoded_tuple_bytes)?;
let bytes = heap.get_tuple(rid)?;
for entry in heap.iter() {
    let (rid, bytes) = entry?;
}
```

`catalog` uses `TableHeap` directly for the catalog's own bootstrap
storage (see `catalog::persist`); `executor::SeqScanExecutor` drives
`TableHeap::scan_page` one step at a time rather than `iter()`, so it
never holds a page pinned across the executor's own pull-based iteration
boundary; `executor::InsertExecutor` calls `insert_tuple` for each
evaluated row.
