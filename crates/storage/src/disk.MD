# disk

`DiskManager`, which owns the database file and performs raw,
page-granular I/O against it. This is the only module allowed to call
into `std::fs` for the main database file; every layer above (buffer
pool, heap, B+tree) reads and writes pages through here rather than
touching the file directly.

## Key Components

- `MAGIC` - the 8-byte magic string identifying a FerroDB database file,
  stored at the start of page 0, just after page 0's own checksum and
  `page_lsn` prefixes.
- `HEADER_VERSION` - the on-disk format version this build reads and
  writes: `11`. Covers not just the page-0 header's own byte layout but the
  heap page layout too (`heap::NO_NEXT_PAGE`, `heap::SlottedPage::init`),
  both baked into every page already on disk, so a change to either must
  bump this, otherwise a file written by an older build gets silently
  misread rather than rejected with a clear error. History: bumped to `4`
  when the header stopped storing `page_count` (`next_page_id` is now
  derived from the file's actual length at open time, so `allocate_page`'s
  `set_len` is the single durable act of allocation instead of racing a
  separate header rewrite against the new page's own initialization);
  to `5` when every page gained an 8-byte `page_lsn` prefix (shrinking
  `MAX_SLOTS`/`MAX_TUPLE_SIZE` accordingly); to `6` when the header gained
  `last_checkpoint_lsn`; to `7` when page 0 itself gained the same 8-byte
  `page_lsn` prefix every other page already had, needed so
  `catalog_first_page`/`last_checkpoint_lsn` can be mutated through the
  ordinary `PageGuard::write` path instead of a raw, unlogged disk write;
  to `8` when every page gained a 4-byte CRC-32 checksum prefix; to `9`
  when the header gained `index_catalog_first_page`, the root of the
  second catalog heap that holds index metadata
  (`catalog::catalog.MD`'s `Catalog::create_index`); to `10` when the
  write-ahead log moved from one bare file to numbered segments
  (`docs/adr/0011-segmented-write-ahead-log.md`) - a pre-segmentation
  `<db_path>.wal`'s first sixteen bytes would otherwise be misread as a
  segment header's magic-plus-start-lsn instead of the old bare eight-byte
  magic, silently reading garbage instead of failing loudly; to `11` when
  the double-write buffer's own header (`dwb::MAGIC`, `FDBDWB02`) grew a
  per-slot CRC (`dwb.MD`) - a data file written before that change could
  still carry an outstanding `FDBDWB01`-format batch that `dwb::read_batch`
  no longer recognizes, and without this bump the data-file version check
  alone would pass while the unreadable batch was silently ignored instead
  of surfacing as a version error. No migration: a file written under an
  older version must be deleted and recreated.
- `header` (`pub(crate)`) - the byte layout of the page-0 header, given as
  six `Range<usize>` constants: `MAGIC_RANGE` (12..20) - the 8-byte magic
  string; `VERSION_RANGE` (20..24) - `HEADER_VERSION`; `PAGE_SIZE_RANGE`
  (28..32) - the page size the database was created with;
  `CATALOG_FIRST_PAGE_RANGE` (24..28) - the table catalog's first page id,
  or `u32::MAX` if none yet; `LAST_CHECKPOINT_LSN_RANGE` (32..40) - the
  last checkpoint's LSN; `INDEX_CATALOG_FIRST_PAGE_RANGE` (40..44) - the
  index catalog's first page id, or `u32::MAX` if no index has ever been
  created, laid out identically to and immediately after
  `CATALOG_FIRST_PAGE_RANGE`'s counterpart but as its own heap rather than
  a row inside the table catalog, since an index's `root_page_id` changes
  every time its B+tree's root splits and a second heap keeps that
  high-churn bookkeeping out of the table catalog's own rows. Zero-padded
  beyond `INDEX_CATALOG_FIRST_PAGE_RANGE` to a full page. `page_count` is
  deliberately not part of this layout - see `HEADER_VERSION`. The ranges
  start at byte 12 rather than 0 because page 0 is an ordinary page like
  any other: bytes 0..4 are its checksum (`page::CHECKSUM_RANGE`, added
  when `HEADER_VERSION` bumped to `8`) and bytes 4..12 are its `page_lsn`
  prefix (`page::PAGE_LSN_RANGE`, added when `HEADER_VERSION` bumped to
  `7`, precisely so that `catalog_first_page`/`last_checkpoint_lsn` could
  start being mutated through the ordinary `PageGuard::write` path) - so
  this layout carries the same version history `HEADER_VERSION` documents,
  not a layout frozen since version 1. `catalog_first_page`,
  `index_catalog_first_page`, and `last_checkpoint_lsn` are mutated
  exclusively through `BufferPool` once the pool exists - fetching page 0
  like any other page and writing
  through `PageGuard::write`, so both become redoable and undoable like
  any other page mutation. `DiskManager` itself only ever *reads* this
  layout, and writes it directly, in full, exactly once: stamping a
  brand-new file's very first header in `write_header`, before there is
  any pool or log to write through.
- `DiskManager` - owns the device, an `AtomicU32 next_page_id`, and a
  `Mutex<u64> extended_len` tracking the file's own last-known length.
  Every method takes `&self`, not `&mut self` (M10.1, `docs/ROADMAP.md`):
  `BlockDevice`'s own methods are `&self` (positioned I/O needs only
  `&File` - see `block_device.MD`), so the only state `DiskManager` itself
  needs to synchronize is page-id allocation and file growth, not the
  device. `DiskManager::open(path, page_size)` opens (creating if
  necessary) the database file at `path`, then takes an exclusive
  advisory lock on it (`block_device::lock_exclusive`) before reading the
  header - held for the `DiskManager`'s whole lifetime, since the lock
  lives on the open file description and is released only when the
  underlying `File` (owned by the `FileDevice` this constructs) is
  dropped. Returns `StorageError::DatabaseLocked` if another process (or
  another `DiskManager::open` call in this one - a second `open()` of the
  same path is a distinct open file description with its own lock state,
  which is what makes this testable within one process) already holds it.
  `DiskManager::open_with_device(device, page_size, path)` opens a
  database file backed by an arbitrary `BlockDevice`, for tests and
  crash-injection fault wrapping, and takes **no** lock at all - the
  crash and in-memory device tests that call it construct devices
  directly and must not need a real file to lock. A
  brand-new (zero-length) device gets a fresh page-0 header written to
  it; an existing one has its header validated (magic and version) and
  `next_page_id` derived from the device's own length - `device_len /
  page_size` - rather than from any stored count, so the device's length
  is the single source of truth for how many pages have been durably
  allocated.
- `DiskManager::read_page_unchecked(&self, page_id, page)` (`pub(crate)`) -
  reads `page_id`'s raw bytes from disk into `page`, without verifying its
  checksum. Used internally by `read_page` (which then verifies) and by
  `recovery::recover_double_write`, which must inspect a possibly-torn
  real page's bytes without that inspection itself turning into a
  `ChecksumMismatch` error - the whole point of that pass is to decide
  what to do about a torn page, not to fail on one. Increments
  `disk_pages_read_total` (a `metrics` counter - see `docs/ROADMAP.md`'s
  M13 entry) here rather than in `read_page`, so every real disk read is
  counted once regardless of which of the two callers made it.
- `DiskManager::read_page(&self, page_id, page)` - reads the page at
  `page_id` from disk into `page`, verifying its CRC-32 checksum unless
  the page is entirely zero. Skipping verification for an all-zero page is
  deliberate: a page whose initializing write never reached disk (e.g. a
  crash between allocating and initializing it) is a valid, empty,
  terminal page in this design (see `heap::NO_NEXT_PAGE`), but the CRC-32
  of 4092 zero bytes is not zero, so without this exception every
  freshly-allocated page would fail verification the first time it was
  read back. Reading a page that was never allocated (at or past the
  file's current extent) is an error rather than yielding zeroed bytes.
- `DiskManager::write_page(&self, page_id, page)` - writes `page`'s
  contents to its slot on disk. Copies the page into a scratch buffer,
  stamps a freshly computed CRC-32 into the scratch copy's
  `CHECKSUM_RANGE`, and writes that - never mutating `page` itself. The
  checksum is derived data, not page content: stamping it into the
  resident frame would be an unlogged mutation of a page the buffer pool
  believes is unchanged, exactly the invariant the write-ahead rule exists
  to prevent. Increments `disk_pages_written_total` once the write
  succeeds.
- `DiskManager::allocate_page(&self) -> Result<PageId, StorageError>` -
  allocates a new page id via `next_page_id.fetch_add`, then extends the
  file to fit it (`extend_to_at_least`). `set_len` is the only durable
  act of allocation - a reopen derives `next_page_id` straight back from
  the file's length, so there is no separate header write to race against
  the new page's own initialization, and no 4KB header rewrite on every
  allocation either.
- `DiskManager::ensure_allocated(&self, page_id)` - extends the file so
  that `page_id` is allocated, if it is not already - i.e. redoes an
  `AllocPage` record. Idempotent: a no-op if `page_id` is already within
  the file's current extent, decided with a compare-exchange loop against
  `next_page_id` rather than a plain load-then-store, so two concurrent
  callers racing to ensure the same or overlapping ranges allocated can't
  both decide they need to bump it. Unlike `allocate_page`, this never
  assigns a *new* id; it only ensures a specific, already-decided one
  physically exists on disk.
- `DiskManager::extend_to_at_least(&self, target_len) -> Result<(), StorageError>` -
  private. Grows the file to `target_len` if it isn't already at least
  that long, tracked through `extended_len` rather than by re-querying
  `device.size()` on every call. The mutex here is what makes
  `allocate_page`/`ensure_allocated` safe under concurrent callers: `next_page_id`'s
  `fetch_add` alone only guarantees distinct ids, not that the *file
  growth* for a smaller id can't lose a race and land after a larger id's
  growth already completed (which would silently truncate the file back
  below a page a concurrent caller was just handed) - tracking the
  high-water mark under the mutex and only ever growing towards it makes
  `set_len` calls monotonic regardless of which caller's `fetch_add` won.
- `DiskManager::sync(&self)` - forces every write made so far to
  durable storage.
- `DiskManager::page_size(&self) -> usize` - the configured page size for
  this database file.
- `DiskManager::offset_of(&self, page_id) -> u64` - private. `page_id.0 as
  u64 * page_size`.
- `DiskManager::write_header(&self)` - private. Stamps a brand-new
  file's very first page-0 header: magic, version, page size, and
  `catalog_first_page`/`index_catalog_first_page`/`last_checkpoint_lsn` all
  at their "none" sentinels (`page_lsn`, right after the checksum, stays
  zero too - the same "never written" sentinel every other fresh page
  has). The only direct header write left in this module - every later
  change to `catalog_first_page`, `index_catalog_first_page`, or
  `last_checkpoint_lsn` goes through `BufferPool` instead, so it is logged
  and can be redone or undone like any other page mutation. Stamps its own
  checksum before writing, same as `write_page`, since this page is real
  (non-zero) content that later reads through `read_page` will verify.
  Relies on the write itself extending a zero-length device to a full
  page (an ordinary write past a file's current end grows it, on every
  platform this crate targets), so `open_with_device`'s "device_len == 0"
  branch seeds `extended_len` at `page_size` up front rather than calling
  `extend_to_at_least` after the fact.

## Usage Example

```rust
use storage::disk::DiskManager;
use storage::page::Page;

let mut disk = DiskManager::open("test.db", 4096)?;
let page_id = disk.allocate_page()?;
let mut page = Page::new(page_id);
page.data_mut()[20..25].copy_from_slice(b"hello");
disk.write_page(page_id, &page)?;
disk.sync()?;
```

`BufferPool::fetch_page`/`allocate_frame` are the only ordinary callers:
they read a page in through `DiskManager::read_page` when it isn't
already resident, and write a dirty frame back through
`DiskManager::write_page` when it's flushed or evicted - a caller above
`storage` never touches `DiskManager` directly. `recovery::recover_double_write`
is the one exception, calling `read_page_unchecked`/`write_page` directly
since it must run *before* any `BufferPool` exists to repair a page a
crash left torn.
