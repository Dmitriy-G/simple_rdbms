# ids

Newtype identifiers for the core entities that flow between subsystems.
Wrapping every raw integer in its own type stops, for example, a
`TableId` from being passed where a `PageId` is expected: the two are both
`u32`s underneath but the compiler will not let them mix.

## Key Components

- `PageId(pub u32)` - identifies a fixed-size page on disk, addressed by
  its offset (in page units, not bytes) from the start of the database
  file.
- `FrameId(pub u32)` - identifies a frame (a page-sized slot) inside the
  buffer pool's in-memory page table, as opposed to the on-disk `PageId` a
  frame currently holds.
- `TxnId(pub u64)` - identifies a transaction for the lifetime of the
  process. Assigned by the `TransactionManager` when a transaction begins.
- `TableId(pub u32)` - identifies a table in the catalog, stable for the
  lifetime of the table.
- `ColumnId(pub u32)` - identifies a column within a table's schema by its
  ordinal position.
- `Lsn(pub u64)` - a log sequence number: the monotonically increasing
  offset of a record in the write-ahead log, used to order log records
  and to tie a page's `pageLSN` back to the log for recovery.
- `Rid { page_id: PageId, slot: u16 }` - a record identifier: the address
  of a tuple as `(page, slot)` within a slotted page. This is how indexes
  and higher layers point at a specific row without copying it.
- `Rid::new(page_id, slot) -> Self` - builds a `Rid` from a page id and
  slot number.

## Usage Example

```rust
use common::{PageId, Rid};

let page = PageId(7);
let rid = Rid::new(page, 3);
assert_eq!(rid.page_id, page);
```

Every subsystem that needs to name a page, frame, transaction, table,
column, log position, or tuple address uses one of these types rather
than a raw integer, so a mismatched id is a compile error instead of a
silent, hard-to-trace bug at runtime.
