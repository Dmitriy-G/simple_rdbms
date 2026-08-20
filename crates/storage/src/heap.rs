use common::{PageId, Rid};

use crate::buffer::BufferPool;
use crate::error::StorageError;
use crate::page::Page;

/// A view over a `Page`'s bytes as a slotted page: a header, a slot array
/// growing forward from just after the header, and tuple bytes packed
/// backward from the end of the page. Slots hold an (offset, length) pair
/// so a `Rid`'s slot number stays stable even as earlier tuples on the page
/// are deleted or resized, which is what lets indexes point at a `Rid`
/// without being invalidated by unrelated writes to the same page.
pub struct SlottedPage<'a> {
    #[allow(dead_code)]
    page: &'a mut Page,
}

impl<'a> SlottedPage<'a> {
    /// Wraps `page` for slotted-page-layout access.
    pub fn new(page: &'a mut Page) -> Self {
        Self { page }
    }

    /// The number of slots currently allocated on the page, including any
    /// occupied by deleted (tombstoned) tuples.
    pub fn slot_count(&self) -> u16 {
        todo!("read the slot count out of the page header")
    }

    /// The raw bytes of the tuple in `slot`, or `None` if the slot is empty
    /// or tombstoned.
    pub fn read(&self, slot: u16) -> Option<&[u8]> {
        let _ = slot;
        todo!("look up the slot's (offset, length) and return that byte range")
    }

    /// Appends `data` as a new tuple, returning its slot number, or `None`
    /// if the page does not have enough free space.
    pub fn insert(&mut self, data: &[u8]) -> Option<u16> {
        let _ = data;
        todo!("grow the slot array, copy data into the free space at the tail, record the slot")
    }

    /// Tombstones `slot`, freeing its bytes for future compaction without
    /// shifting other slots' numbers.
    pub fn delete(&mut self, slot: u16) {
        let _ = slot;
        todo!("mark the slot's entry as deleted, leaving the slot number reserved")
    }
}

/// The on-disk heap: an unordered collection of a table's tuples, stored as
/// a singly-linked chain of slotted pages starting at `first_page_id`. This
/// is the default storage for a table before (or in place of) any index.
pub struct HeapFile<'pool> {
    #[allow(dead_code)]
    buffer_pool: &'pool mut BufferPool,
    #[allow(dead_code)]
    first_page_id: PageId,
}

impl<'pool> HeapFile<'pool> {
    /// Opens the heap file whose first page is `first_page_id`, backed by
    /// `buffer_pool`.
    pub fn open(buffer_pool: &'pool mut BufferPool, first_page_id: PageId) -> Self {
        Self { buffer_pool, first_page_id }
    }

    /// Creates a brand-new, empty heap file (allocates its first page) and
    /// returns a handle to it.
    pub fn create(buffer_pool: &'pool mut BufferPool) -> Result<Self, StorageError> {
        let _ = &buffer_pool;
        todo!("allocate an initial page, initialize it as an empty slotted page")
    }

    /// Inserts `tuple_bytes` into the heap, returning the `Rid` at which it
    /// now lives. Walks the page chain for free space, appending a new page
    /// if none is found.
    pub fn insert(&mut self, tuple_bytes: &[u8]) -> Result<Rid, StorageError> {
        let _ = tuple_bytes;
        todo!("find a page with room, insert via SlottedPage, return its Rid")
    }

    /// Reads the tuple bytes at `rid`.
    pub fn get(&mut self, rid: Rid) -> Result<Vec<u8>, StorageError> {
        let _ = rid;
        todo!("fetch rid.page_id, read the slot, copy its bytes out")
    }

    /// Deletes the tuple at `rid`.
    pub fn delete(&mut self, rid: Rid) -> Result<(), StorageError> {
        let _ = rid;
        todo!("fetch rid.page_id, tombstone the slot")
    }

    /// Returns an iterator over every live tuple in the heap, in physical
    /// page/slot order.
    pub fn iter(&mut self) -> HeapIterator<'_, 'pool> {
        HeapIterator { heap: self }
    }
}

/// Walks a `HeapFile` page by page, slot by slot, yielding `(Rid, Vec<u8>)`
/// pairs for every live tuple. Backs the executor's `SeqScan` operator.
pub struct HeapIterator<'a, 'pool> {
    #[allow(dead_code)]
    heap: &'a mut HeapFile<'pool>,
}

impl Iterator for HeapIterator<'_, '_> {
    type Item = Result<(Rid, Vec<u8>), StorageError>;

    fn next(&mut self) -> Option<Self::Item> {
        todo!("advance the slot cursor, skipping tombstones and crossing page boundaries")
    }
}
