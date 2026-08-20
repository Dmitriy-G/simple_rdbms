//! Newtype identifiers for the core entities that flow between subsystems.
//!
//! Wrapping every raw integer in its own type stops, for example, a
//! `TableId` from being passed where a `PageId` is expected: the two are
//! both `u32`s underneath but the compiler will not let them mix.

/// Identifies a fixed-size page on disk, addressed by its offset (in page
/// units, not bytes) from the start of the database file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PageId(pub u32);

/// Identifies a frame (a page-sized slot) inside the buffer pool's in-memory
/// page table, as opposed to the on-disk `PageId` a frame currently holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FrameId(pub u32);

/// Identifies a transaction for the lifetime of the process. Assigned by the
/// `TransactionManager` when a transaction begins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TxnId(pub u64);

/// Identifies a table in the catalog, stable for the lifetime of the table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TableId(pub u32);

/// Identifies a column within a table's schema by its ordinal position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ColumnId(pub u32);

/// A log sequence number: the monotonically increasing offset of a record in
/// the write-ahead log, used to order log records and to tie a page's
/// `pageLSN` back to the log for recovery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Lsn(pub u64);

/// A record identifier: the address of a tuple as `(page, slot)` within a
/// slotted page. This is how indexes and higher layers point at a specific
/// row without copying it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Rid {
    /// The page the tuple lives on.
    pub page_id: PageId,
    /// The slot within that page's slot array.
    pub slot: u16,
}

impl Rid {
    /// Builds a `Rid` from a page id and slot number.
    pub fn new(page_id: PageId, slot: u16) -> Self {
        Self { page_id, slot }
    }
}
