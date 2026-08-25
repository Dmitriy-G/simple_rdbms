# storage

The storage engine: fixed-size page I/O, buffer pool management, the
on-disk heap file format, index structures, and write-ahead logging — the
layer everything durable and transactional is built on.

## Architecture

`storage` sits above `common` and `types` and below everything that needs
durable, transactional page storage — `catalog`, `txn`, and, through
`executor`, the rest of the query engine (see
`docs/adr/0002-crate-splitting.md`). It is the only crate in the workspace
allowed to call into `std::fs` for the main database file; every layer
above reads and writes pages through it rather than touching the file
directly.

The crate's whole shape follows from one path: how a page mutation becomes
durable without ever risking a torn or half-applied write. `PageGuard::write`
(`buffer.rs`, alongside `Page` in `page.rs`) is the only way to mutate a
resident page's bytes — it appends an `Update` log record (before-image for
undo, new bytes for redo) *before* touching the page, then applies the
bytes and stamps the page's `page_lsn` with the record's own LSN. Nothing
above `storage` can reach a page's bytes any other way, so this ordering —
log first, mutate second — is enforced at the type level, not by
convention.

That guarantee is only worth as much as what happens when the page actually
reaches disk. `BufferPool::flush_pages` enforces the write-ahead rule at
flush time: the log must be durable up to the *batch's* highest `page_lsn`
before any page in the batch is written anywhere. Only once that holds does
the batch get staged through the double-write buffer (`dwb.rs`) — every
page's image written to `<db_path>.dwb` and synced — so the batch is
recoverable even if the crash lands before a single real page is touched.
Only after that does `flush_pages` write the real pages and sync them, then
clear the double-write buffer. A crash at any point along this sequence
leaves either a real page untouched by the batch, or a torn real page with
an intact double-write copy `recovery::recover_double_write` can restore
from — never a torn page with no way back. See
`docs/adr/0005-double-write-buffer-durability-boundary.md` for exactly what
this does and does not cover, and `docs/adr/0003-index-after-the-log.md`
for why the B+tree (`btree.rs`) was sequenced after this durability story
existed rather than before it.

On open, `recovery::recover_double_write` runs first (repairing anything
the double-write buffer can), then `recovery::recover` runs ARIES
Analysis/Redo/Undo against the write-ahead log (`wal.rs`) to bring the
buffer pool's view of the file back to a consistent state before any
caller is allowed to read or write a page.

Unlike the rest of the workspace, `storage` does not
`#![forbid(unsafe_code)]` wholesale — the buffer pool's frame table needs
raw pointer access to page bytes. Every `unsafe` block goes through an
`unsafe fn` boundary explicitly (`#![deny(unsafe_op_in_unsafe_fn)]`) and
carries a `// SAFETY:` comment; today all six live in `buffer.rs`, each
resting on the pin-count protocol described there.

## Key Components

- `block_device` - the lowest layer both `DiskManager` and `LogManager`
  write through: `BlockDevice`, `FileDevice`, and the crash-injection
  fault-wrapping devices used by tests. See
  [block_device.MD](src/block_device.MD).
- `btree` - `BTreeIndex`, a disk-resident B+tree index: node layout,
  `create`/`open`/`get`/`range_scan` (read path), and `insert` with leaf
  and internal splits. `delete` is still `todo!()`. See
  [btree.MD](src/btree.MD).
- `buffer` - `BufferPool`, mediating every page access between the disk
  manager and the layers above. See [buffer.MD](src/buffer.MD).
- `disk` - `DiskManager`, raw page-granular I/O against the database file.
  See [disk.MD](src/disk.MD).
- `dwb` - `DoubleWriteBuffer`, the mechanism that lets a page torn by a
  crash mid-write be repaired instead of merely detected. See
  [dwb.MD](src/dwb.MD).
- `error` - `StorageError`, the crate's error type. See
  [error.MD](src/error.MD).
- `heap` - `SlottedPage` and `TableHeap`, the on-disk slotted-page heap
  file format. See [heap.MD](src/heap.MD).
- `page` - `Page` and `PageGuard`, the fixed-size unit of I/O and its
  checksum/`pageLSN` layout. See [page.MD](src/page.MD).
- `recovery` - ARIES crash recovery: Analysis, Redo, and Undo, plus the
  double-write restore pass that must run before any of it. See
  [recovery.MD](src/recovery.MD).
- `replacer` - `Replacer` and `LruKReplacer`, the buffer pool's eviction
  policy. See [replacer.MD](src/replacer.MD).
- `wal` - `LogManager`, the write-ahead log. See [wal.MD](src/wal.MD).

## Features

Durable page I/O, a bounded buffer pool with LRU-K eviction, slotted-page
heap files, write-ahead logging, ARIES crash recovery, and double-write
protection against torn pages all work today and are exercised end to end
by `crates/engine/tests/crash_injection.rs`.

The B+tree (`btree.rs`) now supports `create`/`open`, `get`, `range_scan`,
and `insert` with leaf and internal splits (roadmap milestones M9.1–M9.2 —
see `docs/ROADMAP.md`), exercised by `tests/btree.rs` (ascending/
descending/random-permutation insertion, variable-length keys, duplicate
keys spanning a split) and `tests/btree_crash_injection.rs` (a root split
swept across every write point under every `block_device::DurabilityModel`,
driving `BTreeIndex` directly since there is no `CREATE INDEX` yet to
drive it through SQL). `delete` is still `todo!()`, and no caller in this
workspace constructs a `BTreeIndex` yet — `catalog` and `executor` both
still route every table through `heap::TableHeap` only, and turning a real
column `Value` into an index key via `types::MemcomparableEncode` is
roadmap milestone M9.3. Until then, sequential scan is the only access
path a real query can take.

## Dependencies

Workspace: `common`, `types`. External: `thiserror`, for `StorageError`;
`rand`, used by the crash-injection test devices in `block_device.rs` to
seed which sectors of a simulated torn write actually land, and by
`tests/btree.rs` to shuffle a seeded random key permutation. Dev-only:
`tempfile`, `proptest`.

This crate also depends on itself with the `test-util` feature enabled
(see Configuration) so its own `tests/` integration tests can see
`BufferPool`'s test-only instrumentation through a normal, non-`--cfg test`
build — the same situation any other crate's tests are in when they depend
on `storage`.

## Configuration

- `test-util` (Cargo feature, off by default) - exposes `BufferPool`'s
  fetch counter and write-observation log to other crates' tests, since
  `#[cfg(test)]` alone only applies when compiling this crate's own test
  binaries.
- `DoubleWriteBuffer::DEFAULT_CAPACITY` - 64 page-image slots, mirrored by
  `common::DbConfig::DEFAULT_DWB_CAPACITY`.
- `page::PAGE_SIZE` - `4096`, matching
  `common::DbConfig::DEFAULT_PAGE_SIZE`. Fixed per database file at
  creation time; `DiskManager::open` rejects a reopen that requests a
  different size than what's recorded in the page-0 header.

Buffer pool size, checkpoint byte threshold, and DWB capacity are runtime
knobs, not constants here — they're threaded in from `common::DbConfig` by
whichever caller constructs a `BufferPool`/`DoubleWriteBuffer` (in the
ordinary path, `engine::Database::open`).

## Testing

Everything is covered under `tests/`, exercised entirely through this
crate's public API: `disk_manager.rs` (page round-trips across a reopen,
header validation), `buffer_pool.rs` (eviction under pressure, dirty-data
durability, pinning exhaustion), `double_write_buffer.rs` (batch
round-trip, corrupted-slot detection), `wal.rs` (record round-trip,
`prev_lsn` chaining, truncation at a torn record), `recovery.rs`
(Analysis/Redo/Undo against hand-built logs), `undo_performance.rs`
(undo's bounded I/O cost per record, via the `CountingDevice` helper in
`tests/support/mod.rs`), `slotted_page.rs` (over-capacity insertion),
`table_heap.rs` (multi-page insert/read-back, oversized-tuple rejection),
`btree.rs` (insert/split correctness: ascending/descending/random-
permutation insertion, variable-length keys, oversized-key rejection,
root-height growth, duplicate keys spanning a split, ordered range scans),
and `btree_crash_injection.rs` (a root split swept across every write
point under every `DurabilityModel`, driving `BTreeIndex` directly against
fault-injecting devices). `tests/smoke.rs` is the minimum-viable
compile-and-construct check. A
`#[cfg(test)]` unit test in `src/` is reserved for the rare case that
needs access to something that should stay private (see CLAUDE.md's
testing section); none of this crate's own `src/` currently does. Run
just this crate with:

```sh
cargo test -p storage
```
