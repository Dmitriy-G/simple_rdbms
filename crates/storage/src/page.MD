# page

`Page`, the fixed-size unit of I/O between the disk manager and the buffer
pool and the unit of layout for slotted-page heap storage and the future
B+tree, plus the two byte ranges every page layout built on top of it must
treat as reserved: a CRC-32 checksum and a `pageLSN`. `PageGuard`, defined
here alongside `Page` since it needs access to `BufferPool`'s private frame
table, is the RAII handle that makes a pin release exactly once, at the
type level.

## Key Components

- `PAGE_SIZE` - the fixed size, in bytes, of every page in the database
  file and the write-ahead log: `4096`, matching
  `common::DbConfig::DEFAULT_PAGE_SIZE`. A database file's actual page
  size is recorded in the page-0 header at creation time, and
  `DiskManager::open` rejects a reopen whose requested page size
  disagrees with it, so this is not meant to vary per-page or across
  reopens of the same file.
- `CHECKSUM_RANGE` - `0..4`, the byte range at the very start of every
  page reserved for a CRC-32 checksum over the rest of the page's bytes.
  Computed and verified at the `DiskManager` layer
  (`write_page`/`read_page`), not by any layout built on top of `Page`,
  so it is format-agnostic: the slotted heap page format gets it today,
  and the B+tree's node pages will get it for free once implemented, with
  no changes to that code. An all-zero page skips verification instead of
  failing it - see `checksum_ok`.
- `PAGE_LSN_RANGE` - `4..12`, just after `CHECKSUM_RANGE`, reserved for the
  page's `pageLSN`: the log sequence number of the last WAL record that
  describes a change to this page's bytes. The write-ahead rule is
  enforced by comparing this against the log's own durable LSN before the
  page is allowed to reach disk (see `BufferPool::flush_pages`), so every
  page layout built on top of `Page` (slotted heap pages, B+tree nodes)
  must treat these 8 bytes, together with `CHECKSUM_RANGE`, as reserved,
  not available for its own layout.
- `checksum_of(bytes) -> u32` - the CRC-32 checksum `bytes` should hold in
  `CHECKSUM_RANGE` for its content to be intact: computed over everything
  past the checksum field itself (`CHECKSUM_RANGE.end..PAGE_SIZE`).
- `stamp_checksum(bytes)` - recomputes `bytes`'s checksum and stamps it
  into `CHECKSUM_RANGE`.
- `checksum_ok(bytes) -> bool` - whether `bytes` is a complete,
  uncorrupted page: either all zero (the sentinel for a page whose
  initializing write never reached disk - see `heap::NO_NEXT_PAGE`'s own
  entry for why that's deliberately valid, not corrupt) or its stored
  checksum matches `checksum_of`. Shared by `DiskManager::read_page`
  (which reports a mismatch as `StorageError::ChecksumMismatch`) and the
  double-write buffer's recovery pass (`recovery::recover_double_write`),
  which instead needs to *decide* what to do with a bad copy rather than
  fail on one.
- `Page` - a single fixed-size page. Holds raw bytes; interpreting them
  (as a heap page, an index node, etc.) is the job of the module that owns
  that layout. `Page::new(id)` creates a zeroed page for the given id.
  `Page::id(&self) -> PageId` returns its id. `Page::data(&self) -> &[u8;
  PAGE_SIZE]` gives read-only access. `Page::data_mut(&mut self) -> &mut
  [u8; PAGE_SIZE]` gives mutable access - not exposed outside the crate as
  a way to mutate a *resident* page's bytes (`PageGuard::write` is the
  only path for that, since it is what logs the change before applying
  it); it stays `pub` for callers building a `Page` from scratch off the
  buffer pool, e.g. `DiskManager::read_page`, which fills a freshly
  constructed page from disk with nothing yet to log.
  `Page::page_lsn(&self) -> Lsn` is the LSN of the last WAL record
  describing a change to this page, or `Lsn(0)` if the page has never been
  written through `PageGuard::write` (a fresh, all-zero page).
  `Page::set_page_lsn(&mut self, lsn)` stamps this page with `lsn`; called
  by `PageGuard::write` after applying a change, never directly by higher
  layers.
- `PageGuard<'pool>` - an RAII handle to a page pinned in the buffer pool.
  Borrowing a page through a `PageGuard` (rather than a bare `&Page`) is
  what lets the buffer pool track pin counts: acquiring a guard increments
  the frame's pin count, and dropping it decrements that count, making
  the frame eligible for eviction again once nothing holds a guard to it.
  It is the *only* way to release a pin: it has no public constructor
  (its fields are `pub(crate)`) and `BufferPool` exposes no manual unpin
  method, so the sole way to release a pin is to drop the guard that holds
  it - a caller cannot double-release a pin, because there is no second
  operation left to call. `PageGuard::page_id(&self) -> PageId` returns
  the id of the page this guard pins. `page()`, `write()`, and `Drop` live
  in `buffer.rs` alongside `BufferPool` instead, since they need access to
  its private frame table, pin counts, dirty flags, and log manager.

## Usage Example

```rust
use storage::page::{Page, checksum_ok, stamp_checksum};

let mut page = Page::new(common::PageId(1));
page.data_mut()[20..25].copy_from_slice(b"hello");
let mut scratch = *page.data();
stamp_checksum(&mut scratch);
assert!(checksum_ok(&scratch));
```

`DiskManager::write_page` stamps a scratch copy's checksum before writing
it to disk, never the resident page itself - the checksum is derived data,
not page content, and stamping it into a frame `BufferPool` believes is
unchanged would be an unlogged mutation, exactly what the write-ahead rule
exists to prevent. `DiskManager::read_page` calls `checksum_ok` on every
page it reads back, turning a mismatch into `StorageError::ChecksumMismatch`.
Every higher layer reaches a resident page's bytes only through a
`PageGuard`, obtained from `BufferPool::fetch_page`/`new_page` and
released by dropping it.
