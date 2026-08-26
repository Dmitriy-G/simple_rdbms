# error

Errors raised by the storage engine: disk I/O, buffer pool exhaustion,
corrupt page layout, or WAL failures. This is the crate's one error type -
every fallible operation in `storage` returns `Result<_, StorageError>`,
and `catalog`/`txn`/`executor` each convert it into their own local error
type at their boundary (see `common::Error`, which every variant here maps
onto by hand - `ChecksumMismatch`/`CorruptPage`/`CorruptLogHeader`/
`TruncatedFile`/`PageNotFound` all become a data-corruption `common::Error`
variant, `BufferPoolExhausted`/`TupleTooLarge`/`KeyTooLarge`/
`DoubleWriteRestoreFailed`/`InvalidDwbCapacity` each become their own,
`UnorderableKey` becomes `common::Error::DatatypeMismatch` (the closest
existing condition - this crate has no index-specific `common::Error`
variant of its own to map onto) - see `common`'s `error.MD` for which).

## Key Components

- `StorageError` - the enum. `Io` wraps a filesystem/OS-level I/O failure,
  converting automatically via `#[from] std::io::Error`.
  `BufferPoolExhausted` means every resident frame is pinned when a new
  page must be brought in. `CorruptPage { page_id, reason }` is a
  structural sanity-check failure (bad magic, truncated slot array, etc.)
  - not a checksum mismatch, which has its own variant. `PageNotFound`
    names a page id that doesn't exist in the file. `TruncatedFile {
    actual, expected, page_size }` means the database file's length isn't
    a whole multiple of the page size, so the last page is partially
    written; since `allocate_page`'s `set_len` is a single syscall this
    should only arise from damage outside the engine's own writes, but is
    still reported clearly rather than silently truncated or rounded.
  `TupleTooLarge { size, max }` means a tuple's encoded bytes are larger
  than a single page can ever hold, even empty - no amount of retrying
  onto a fresh page would help, so this is raised immediately rather than
  looping. `KeyTooLarge { size, max }` is `btree::BTreeIndex::insert`'s
  equivalent check for an index key: raised before any page is touched if
  the key's encoded bytes could never fit an empty leaf node, for the same
  reason `TupleTooLarge` is checked up front rather than discovered
  partway through a split. `CorruptLogHeader { reason }` means the WAL's own fixed magic
  bytes failed validation, or the file is shorter than the header.
  `ChecksumMismatch { page_id, expected, actual }` means a page's stored
  CRC-32 doesn't match the one recomputed over its bytes at read time -
  the page was altered by something other than `DiskManager::write_page`
  (media corruption, a torn write, disk bit rot); by the time this
  survives to `recovery::recover`'s own Redo pass, `recover_double_write`
  has already repaired every torn page it could, so this is a hard,
  unrecoverable error, not something to work around. `DoubleWriteRestoreFailed
  { page_id }` means `recovery::recover_double_write` restored a page from
  its double-write copy but the page still fails its own checksum once
  read back - the restore write itself was interrupted (e.g. a second
  crash during recovery) - so the batch is left in place rather than
  cleared, letting a later recovery attempt retry from the still-intact
  copy instead of losing it. `InvalidDwbCapacity` means
  `DoubleWriteBuffer::open`'s `capacity` argument was zero: a double-write
  buffer with no page-image slots cannot protect any page, and
  `BufferPool::flush_pages`'s `pages.len() <= capacity` assertion would
  debug-panic on the very first non-empty flush rather than failing
  cleanly at construction time. `UnorderableKey` wraps a `types::ValueError`
  (`types::memcomparable.MD`), converting automatically via `#[from]`: a
  caller that builds an index key with
  `types::MemcomparableEncode::encode_memcomparable` and propagates its
  `Result` with `?` gets a `StorageError` for free, so a value with no
  defined ordering (today, only a NaN `Double`) fails the statement before
  `btree::BTreeIndex::insert` ever sees it, instead of being encoded into a
  key that would corrupt the index's ordering silently. `btree` itself
  never calls `encode_memcomparable` - it stays ignorant of `Value`
  entirely (see `btree.MD`) - so this variant exists for the caller that
  will build keys ahead of `insert`, once M9.3 wires an index into the
  query path.
- `impl From<StorageError> for common::Error` - matches each `StorageError`
  variant to the `common::Error` variant naming the same condition,
  preserving its structured fields rather than collapsing them into a
  formatted string; the single point where this crate's error type crosses
  into the shared one every crate above it matches on.

## Usage Example

```rust
use storage::StorageError;

fn open_page(disk: &mut storage::disk::DiskManager, id: common::PageId) -> Result<(), StorageError> {
    let mut page = storage::page::Page::new(id);
    disk.read_page(id, &mut page)?;
    Ok(())
}
```

Every public function in this crate that can fail returns
`Result<_, StorageError>`; callers above `storage` (`catalog::CatalogError`,
`txn::TxnError`, `executor::ExecutorError`) each wrap it with `#[from]` into
their own local error, and `common::Error` is where all of those eventually
converge for `engine`/`cli` to match on.
