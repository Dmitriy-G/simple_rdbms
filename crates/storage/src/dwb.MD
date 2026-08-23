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
  file's header page (page 0 of the `.dwb` file).
- `ENTRY_COUNT_OFFSET`/`PAGE_IDS_OFFSET` - byte offsets, within the header
  page, of the `u32` entry count and the batch's page ids.
- `DoubleWriteBuffer` - a separate file holding, at any moment, either
  nothing or one in-flight batch of page images: an exact copy of every
  page a `BufferPool` flush is about to write to the real data file, made
  durable *before* any of those real writes happen (see
  `BufferPool::flush_pages` for how the full six-step protocol - write
  batch, sync, write real pages, sync, clear batch, sync - uses this, and
  `recovery::recover_double_write` for how a leftover batch repairs a torn
  real page on reopen). Page 0 of the file is the header: `MAGIC` (8
  bytes) | `entry_count` (`u32`) | `entry_count` page ids (`u32` each) | a
  CRC-32 (`u32`) over every byte before it, zero-padded to a full page.
  Pages `1..=capacity` are page-image slots, each an ordinary checksummed
  `Page` image - a torn slot write is self-detecting the same way a torn
  real-file write is, which is exactly what lets recovery tell "the crash
  landed while this copy was still being written" (bad checksum: skip it,
  the real page was never touched) apart from "this copy is intact" (good
  checksum: it's safe to restore from).
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
- `DoubleWriteBuffer::write_batch(&mut self, pages)` - protocol steps 1-3:
  writes every page in `pages` into its own slot (in order, each
  independently checksummed), writes the header recording this batch's
  page ids, then `sync_all`s the whole file. After this returns
  (successfully), the batch is durable and recoverable even if the caller
  crashes before writing a single one of these pages to its real
  location.
- `DoubleWriteBuffer::clear_batch(&mut self)` - protocol step 6: marks the
  batch retired (`entry_count = 0`) and `sync_all`s. Called once every
  page in the batch has been written to its real location and that write
  is itself durable (step 5) - after this, the slot images are stale and
  no longer needed.
- `DoubleWriteBuffer::encode_header(&self, page_ids) -> [u8; PAGE_SIZE]` -
  private. Builds the header page bytes for `write_batch`/`clear_batch`.
- `DoubleWriteBuffer::read_batch(&mut self) -> Result<Option<Vec<PageId>>, StorageError>` -
  recovery: decodes the header page, returning the batch's page ids in
  slot order, or `None` if there is nothing to recover - bad magic, an
  `entry_count` of `0` or above `capacity`, or a CRC mismatch are all
  treated alike, as "invalid," per the recovery protocol: none of them
  are errors, since a header can legitimately be caught mid-write by the
  very crash recovery is running to clean up after.
- `DoubleWriteBuffer::read_slot(&mut self, index) -> Result<[u8; PAGE_SIZE], StorageError>` -
  recovery: the raw bytes of slot `index` (0-based, matching the order
  `read_batch` returned page ids in), whether or not they pass their own
  checksum - the caller (`recovery::recover_double_write`) is the one that
  decides what a bad copy means.
- `read_u32(buf, at) -> u32` - private helper.

## Usage Example

```rust
use storage::dwb::DoubleWriteBuffer;

let mut dwb = DoubleWriteBuffer::open("test.db.dwb", DoubleWriteBuffer::DEFAULT_CAPACITY)?;
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
