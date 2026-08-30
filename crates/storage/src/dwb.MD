# dwb

The double-write buffer: a separate `<db_path>.dwb` file that gives
recovery an intact copy of every page about to be written to the real
data file, so a page torn by a crash mid-write (a single `write_at` is
not atomic on real hardware) can be restored instead of merely detected.
Deliberately a separate file, not a region of the main database file:
page ids are baked into every WAL record already, so nothing here may
renumber or otherwise shift them. See `docs/adr/0005-double-write-buffer-durability-boundary.md`
for the precise durability claim this mechanism makes.

## Key Components

- `MAGIC` - the 8-byte magic string identifying a double-write buffer
  file's header page (page 0 of the `.dwb` file). `FDBDWB02` - bumped from
  `FDBDWB01` when the header grew a per-slot CRC (below); an old-format
  header's bytes never match the new magic, so `read_batch` reports
  nothing to recover rather than misinterpreting a 4-byte-per-entry header
  as the new 8-byte-per-entry layout.
- `ENTRY_COUNT_OFFSET`/`ENTRIES_OFFSET`/`ENTRY_SIZE` - byte offsets and
  stride, within the header page, of the `u32` entry count and the
  batch's `(page_id, slot_crc)` entries. `ENTRY_SIZE` is `8` - a `u32`
  page id followed by a `u32` CRC-32 of that slot's exact on-disk bytes.
- `DoubleWriteBuffer` - wraps its `Box<dyn BlockDevice>` in a `Mutex`
  (M10.1, `docs/ROADMAP.md`): every method takes `&self` now, and while
  `BufferPool::flush_pages` already serializes actual flush batches (so
  the mutex is never contended in practice), the type itself must be
  `Send + Sync` regardless. A separate file holding, at any moment, either
  nothing or one in-flight batch of page images: an exact copy of every
  page a `BufferPool` flush is about to write to the real data file, made
  durable *before* any of those real writes happen (see
  `BufferPool::flush_pages` for how the full six-step protocol - write
  batch, sync, write real pages, sync, clear batch, sync - uses this, and
  `recovery::recover_double_write` for how a leftover batch repairs a torn
  real page on reopen). Page 0 of the file is the header: `MAGIC` (8
  bytes) | `entry_count` (`u32`) | `entry_count` entries, each a page id
  (`u32`) paired with that slot's CRC-32 (`u32`) | a CRC-32 (`u32`) over
  every byte before it, zero-padded to a full page. Pages `1..=capacity`
  are page-image slots, each an ordinary checksummed `Page` image, plus
  now the header's own per-slot `slot_crc` binding each slot to the exact
  bytes it held the moment `write_batch` wrote it - see `read_batch`
  below for why the slot's own internal page checksum was never enough
  on its own to answer "does slot `i` actually hold `page_ids[i]`."
- `DoubleWriteBuffer::DEFAULT_CAPACITY` - `64`. The default number of
  page-image slots: how many pages a single flush batch can cover before
  `BufferPool::flush_all` must split it into more than one batch.
- `DoubleWriteBuffer::open(path, capacity)` - opens (creating if
  necessary) the double-write buffer file at `path`, sized for `capacity`
  page-image slots. Rejects `capacity == 0` with
  `StorageError::InvalidDwbCapacity` rather than constructing a buffer
  `flush_pages` would debug-panic against on its first non-empty batch.
- `DoubleWriteBuffer::open_with_device(device, capacity)` - opens a
  double-write buffer backed by an arbitrary `BlockDevice`, for tests and
  crash-injection fault wrapping. Only ensures the device is large enough
  for its header page plus `capacity` slots, growing it via `set_len` if
  short (the same zero-fill semantics `DiskManager` already relies on) -
  it deliberately never reads or validates the header page's content. An
  all-zero header page already decodes as "bad magic, nothing to recover"
  in `read_batch`, the same reasoning that makes an all-zero heap page a
  valid empty one (see `heap::NO_NEXT_PAGE`) rather than requiring its own
  special-cased initialization here.
- `DoubleWriteBuffer::capacity(&self) -> usize` - the number of
  page-image slots this double-write buffer has.
- `DoubleWriteBuffer::offset_of_slot(&self, index) -> u64` - private.
  `(1 + index) * PAGE_SIZE`.
- `DoubleWriteBuffer::write_batch(&self, pages)` - protocol steps 1-3:
  writes every page in `pages` into its own slot (in order, each
  independently checksummed), computes a CRC-32 over each slot's exact
  on-disk bytes as it writes it, writes the header recording this batch's
  `(page_id, slot_crc)` entries, then `sync_all`s the whole file. After
  this returns (successfully), the batch is durable and recoverable even
  if the caller crashes before writing a single one of these pages to its
  real location.
- `DoubleWriteBuffer::clear_batch(&self)` - protocol step 6: marks the
  batch retired (`entry_count = 0`) and `sync_all`s. Called once every
  page in the batch has been written to its real location and that write
  is itself durable (step 5) - after this, the slot images are stale and
  no longer needed.
- `DoubleWriteBuffer::encode_header(&self, entries) -> [u8; PAGE_SIZE]` -
  private. Builds the header page bytes for `write_batch`/`clear_batch`
  from an iterator of `(PageId, u32)` entries.
- `DoubleWriteBuffer::read_batch(&self) -> Result<Option<Vec<(PageId, u32)>>, StorageError>` -
  recovery: decodes the header page, returning the batch's
  `(page_id, slot_crc)` entries in slot order, or `None` if there is
  nothing to recover - bad magic (including an old-format header, since
  its magic never matches `MAGIC`), an `entry_count` of `0` or above
  `capacity`, or a header CRC mismatch are all treated alike, as
  "invalid," per the recovery protocol: none of them are errors, since a
  header can legitimately be caught mid-write by the very crash recovery
  is running to clean up after - the "all-zero pages are valid" invariant
  (`CLAUDE.md`) applies here too, since a never-written header is all
  zeros and trivially fails the magic check the same way a torn one would.
- `DoubleWriteBuffer::read_slot(&self, index) -> Result<[u8; PAGE_SIZE], StorageError>` -
  recovery: the raw bytes of slot `index` (0-based, matching the order
  `read_batch` returned entries in), whether or not they pass their own
  checksum or their recorded `slot_crc` - the caller
  (`recovery::recover_double_write`) is the one that decides what a bad
  copy means.
- `DoubleWriteBuffer::write_raw_slot(&self, index, content)`/
  `write_raw_header_entries(&self, entries)` - `#[cfg(any(test, feature =
  "test-util"))]`. Write a slot's raw bytes, or an arbitrary
  `(page_id, slot_crc)` entry list as a well-formed (correct magic,
  count, and header CRC) header, bypassing `write_batch`'s automatic
  slot/entry pairing entirely. Exist so `crates/storage/tests/double_write_buffer.rs`
  can construct the specific inconsistent states `recover_double_write`'s
  hardening checks exist to catch - a slot whose content doesn't match
  its recorded `slot_crc`, an all-zero slot, or a slot that actually
  holds a different entry's content than the header claims - without the
  test needing to know this module's private byte offsets itself.
- `read_u32(buf, at) -> u32` - private helper.

## Usage Example

```rust
use storage::dwb::DoubleWriteBuffer;

let dwb = DoubleWriteBuffer::open("test.db.dwb", DoubleWriteBuffer::DEFAULT_CAPACITY)?;
dwb.write_batch(&[page_a, page_b])?;
// ... write page_a and page_b to their real locations, then sync ...
dwb.clear_batch()?;
```

`BufferPool::flush_pages` is the only caller of `write_batch`/`clear_batch`
in the ordinary path, driving the full six-step protocol on every dirty
batch it flushes. `recovery::recover_double_write` is the only caller of
`read_batch`/`read_slot`, run once at `Database::open`, before any
`BufferPool` exists, to repair whatever a leftover in-flight batch says
about a page a crash may have torn.
