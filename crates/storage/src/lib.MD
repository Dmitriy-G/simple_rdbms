# lib

The storage engine: fixed-size page I/O, buffer pool management, the
on-disk heap file format, index structures, and write-ahead logging. Sits
above `common` and `types` in the dependency graph (see
`docs/adr/0002-crate-splitting.md`) and below everything that needs
durable, transactional page storage — `catalog`, `txn`, and, through
`executor`, the rest of the query engine.

Unlike the rest of the workspace, `storage` does not `#![forbid(unsafe_code)]`
wholesale, because the buffer pool and page layout will eventually need
raw pointer access to page bytes for performance. Any `unsafe` block that
is added must go through an `unsafe fn` boundary explicitly, enforced by
`#![deny(unsafe_op_in_unsafe_fn)]`; six such blocks exist today, all in
`buffer.rs`, each justified by a `// SAFETY:` comment at the point it's
checked.

## Key Components

- `block_device` - the lowest layer both `DiskManager` and `LogManager`
  write through: `BlockDevice`, `FileDevice`, and the crash-injection
  fault-wrapping devices used by tests. See `block_device.MD`.
- `btree` - disk-resident B+tree index structures (currently a stub whose
  methods are `todo!()`). See `btree.MD`.
- `buffer` - `BufferPool`, mediating every page access between the disk
  manager and the layers above. See `buffer.MD`.
- `disk` - `DiskManager`, raw page-granular I/O against the database file.
  See `disk.MD`.
- `dwb` - `DoubleWriteBuffer`, the M12 mechanism that lets a page torn by a
  crash mid-write be repaired instead of merely detected. See `dwb.MD`.
- `StorageError` - the crate's error type, re-exported at the crate root
  (the module itself, `error`, stays private). See `error.MD`.
- `heap` - `SlottedPage` and `TableHeap`, the on-disk slotted-page heap
  file format. See `heap.MD`.
- `page` - `Page` and `PageGuard`, the fixed-size unit of I/O and its
  checksum/`pageLSN` layout. See `page.MD`.
- `recovery` - ARIES crash recovery: Analysis, Redo, and Undo, plus the
  double-write restore pass that must run before any of it. See
  `recovery.MD`.
- `replacer` - `Replacer` and `LruKReplacer`, the buffer pool's eviction
  policy. See `replacer.MD`.
- `wal` - `LogManager`, the write-ahead log. See `wal.MD`.

## Usage Example

```rust
use storage::buffer::BufferPool;
use storage::disk::DiskManager;
use storage::dwb::DoubleWriteBuffer;
use storage::recovery;
use storage::replacer::LruKReplacer;
use storage::wal::LogManager;

let mut disk = DiskManager::open("db.file", 4096)?;
let mut dwb = DoubleWriteBuffer::open("db.file.dwb", DoubleWriteBuffer::DEFAULT_CAPACITY)?;
recovery::recover_double_write(&mut disk, &mut dwb)?;

let log = LogManager::open("db.file.wal")?;
let pool = BufferPool::new(disk, dwb, log, 64, Box::new(LruKReplacer::new(64, 2)));
recovery::recover(&pool)?;
```

`engine::Database::open` is the sole assembler of this stack in the
ordinary path: it opens a `DiskManager`, `DoubleWriteBuffer`, and
`LogManager` on real files, runs `recovery::recover_double_write` and
`recovery::recover` before constructing anything above `storage`, and only
then builds the `BufferPool` everything else (`catalog`, `txn`,
`executor`) is handed.
