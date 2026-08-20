use common::{PageId, Rid};

use crate::buffer::BufferPool;
use crate::error::StorageError;
use crate::page::Page;

/// Sentinel stored in a slotted page's `next_page_id` field meaning "this is
/// the last page in the chain."
const NO_NEXT_PAGE: PageId = PageId(u32::MAX);

/// Byte layout of a slotted page's 8-byte header.
const SLOT_COUNT_RANGE: std::ops::Range<usize> = 0..2;
const FREE_SPACE_END_RANGE: std::ops::Range<usize> = 2..4;
const NEXT_PAGE_ID_RANGE: std::ops::Range<usize> = 4..8;
const HEADER_SIZE: usize = 8;
/// Each slot is a (u16 offset, u16 len) pair.
const SLOT_SIZE: usize = 4;

fn slot_offset(slot: u16) -> usize {
    HEADER_SIZE + slot as usize * SLOT_SIZE
}

fn read_u16(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn write_u16(bytes: &mut [u8], at: usize, value: u16) {
    bytes[at..at + 2].copy_from_slice(&value.to_le_bytes());
}

/// Reads the number of slots (live or tombstoned) allocated on a slotted
/// page's raw bytes.
fn slotted_slot_count(bytes: &[u8]) -> u16 {
    read_u16(bytes, SLOT_COUNT_RANGE.start)
}

/// Reads the offset (from the start of the page) at which tuple data
/// currently begins on a slotted page's raw bytes.
fn slotted_free_space_end(bytes: &[u8]) -> u16 {
    read_u16(bytes, FREE_SPACE_END_RANGE.start)
}

/// Reads the id of the next page in this heap's chain, or `NO_NEXT_PAGE`.
fn slotted_next_page_id(bytes: &[u8]) -> PageId {
    PageId(u32::from_le_bytes([
        bytes[NEXT_PAGE_ID_RANGE.start],
        bytes[NEXT_PAGE_ID_RANGE.start + 1],
        bytes[NEXT_PAGE_ID_RANGE.start + 2],
        bytes[NEXT_PAGE_ID_RANGE.start + 3],
    ]))
}

/// Looks up slot `slot`'s (offset, len) entry, or `None` if `slot` is past
/// the allocated slot array.
fn slotted_slot_entry(bytes: &[u8], slot: u16) -> Option<(u16, u16)> {
    if slot >= slotted_slot_count(bytes) {
        return None;
    }
    let at = slot_offset(slot);
    Some((read_u16(bytes, at), read_u16(bytes, at + 2)))
}

/// The raw bytes of the tuple in `slot` on a slotted page's raw bytes, or
/// `None` if the slot doesn't exist or is tombstoned (`len == 0`).
fn slotted_read(bytes: &[u8], slot: u16) -> Option<&[u8]> {
    let (offset, len) = slotted_slot_entry(bytes, slot)?;
    if len == 0 {
        return None;
    }
    Some(&bytes[offset as usize..offset as usize + len as usize])
}

/// The number of free bytes available for a new slot plus its tuple data.
fn slotted_free_space(bytes: &[u8]) -> usize {
    let slots_end = HEADER_SIZE + slotted_slot_count(bytes) as usize * SLOT_SIZE;
    (slotted_free_space_end(bytes) as usize).saturating_sub(slots_end)
}

/// A view over a `Page`'s bytes as a slotted page: a header, a slot array
/// growing forward from just after the header, and tuple bytes packed
/// backward from the end of the page. Slots hold an (offset, length) pair
/// so a `Rid`'s slot number stays stable even as earlier tuples on the page
/// are deleted or resized, which is what lets indexes point at a `Rid`
/// without being invalidated by unrelated writes to the same page.
pub struct SlottedPage<'a> {
    page: &'a mut Page,
}

impl<'a> SlottedPage<'a> {
    /// Wraps `page` for slotted-page-layout access.
    pub fn new(page: &'a mut Page) -> Self {
        Self { page }
    }

    /// Initializes an empty page's header: no slots, tuple data starts at
    /// the end of the page, and no next page in the chain yet.
    pub fn init(&mut self) {
        let bytes = self.page.data_mut();
        write_u16(bytes, SLOT_COUNT_RANGE.start, 0);
        write_u16(bytes, FREE_SPACE_END_RANGE.start, crate::page::PAGE_SIZE as u16);
        bytes[NEXT_PAGE_ID_RANGE].copy_from_slice(&NO_NEXT_PAGE.0.to_le_bytes());
    }

    /// The number of slots currently allocated on the page, including any
    /// occupied by deleted (tombstoned) tuples.
    pub fn slot_count(&self) -> u16 {
        slotted_slot_count(self.page.data())
    }

    /// The raw bytes of the tuple in `slot`, or `None` if the slot is empty
    /// or tombstoned.
    pub fn read(&self, slot: u16) -> Option<&[u8]> {
        slotted_read(self.page.data(), slot)
    }

    /// The number of free bytes available for a new slot plus its tuple
    /// data.
    pub fn free_space(&self) -> usize {
        slotted_free_space(self.page.data())
    }

    /// The id of the next page in this heap's chain, or `None` if this is
    /// the chain's last page.
    pub fn next_page_id(&self) -> Option<PageId> {
        let id = slotted_next_page_id(self.page.data());
        (id != NO_NEXT_PAGE).then_some(id)
    }

    /// Links this page to `next` as the next page in the chain.
    pub fn set_next_page_id(&mut self, next: PageId) {
        self.page.data_mut()[NEXT_PAGE_ID_RANGE].copy_from_slice(&next.0.to_le_bytes());
    }

    /// Appends `data` as a new tuple, returning its slot number, or `None`
    /// if the page does not have enough free space.
    pub fn insert(&mut self, data: &[u8]) -> Option<u16> {
        let slot_count = self.slot_count();
        let new_slots_end = HEADER_SIZE + (slot_count as usize + 1) * SLOT_SIZE;
        let space_end = slotted_free_space_end(self.page.data()) as usize;
        if new_slots_end + data.len() > space_end {
            return None;
        }

        let data_start = space_end - data.len();
        let bytes = self.page.data_mut();
        bytes[data_start..space_end].copy_from_slice(data);
        write_u16(bytes, FREE_SPACE_END_RANGE.start, data_start as u16);
        let slot_at = slot_offset(slot_count);
        write_u16(bytes, slot_at, data_start as u16);
        write_u16(bytes, slot_at + 2, data.len() as u16);
        write_u16(bytes, SLOT_COUNT_RANGE.start, slot_count + 1);

        Some(slot_count)
    }

    /// Tombstones `slot`, freeing its bytes for future compaction without
    /// shifting other slots' numbers.
    // TODO(M5): vacuum - reclaim tombstoned tuple bytes via compaction.
    pub fn delete(&mut self, slot: u16) {
        if slotted_slot_entry(self.page.data(), slot).is_some() {
            let at = slot_offset(slot);
            write_u16(self.page.data_mut(), at + 2, 0);
        }
    }
}

/// The on-disk heap: an unordered collection of a table's tuples, stored as
/// a singly-linked chain of slotted pages starting at `first_page_id`. This
/// is the default storage for a table before (or in place of) any index.
pub struct HeapFile<'pool> {
    buffer_pool: &'pool BufferPool,
    first_page_id: PageId,
}

impl<'pool> HeapFile<'pool> {
    /// Opens the heap file whose first page is `first_page_id`, backed by
    /// `buffer_pool`.
    pub fn open(buffer_pool: &'pool BufferPool, first_page_id: PageId) -> Self {
        Self { buffer_pool, first_page_id }
    }

    /// Creates a brand-new, empty heap file (allocates its first page) and
    /// returns a handle to it.
    pub fn create(buffer_pool: &'pool BufferPool) -> Result<Self, StorageError> {
        let (page_id, mut guard) = buffer_pool.new_page()?;
        SlottedPage::new(guard.page_mut()).init();
        drop(guard);
        buffer_pool.unpin_page(page_id, true)?;
        Ok(Self { buffer_pool, first_page_id: page_id })
    }

    /// The id of this heap's first page.
    pub fn first_page_id(&self) -> PageId {
        self.first_page_id
    }

    /// Inserts `tuple_bytes` into the heap, returning the `Rid` at which it
    /// now lives. Walks the page chain for free space, appending a new page
    /// if none is found.
    pub fn insert(&mut self, tuple_bytes: &[u8]) -> Result<Rid, StorageError> {
        let mut current = self.first_page_id;
        loop {
            let mut guard = self.buffer_pool.fetch_page(current)?;
            let mut slotted = SlottedPage::new(guard.page_mut());
            if let Some(slot) = slotted.insert(tuple_bytes) {
                drop(guard);
                self.buffer_pool.unpin_page(current, true)?;
                return Ok(Rid::new(current, slot));
            }
            let next = slotted.next_page_id();
            drop(guard);
            self.buffer_pool.unpin_page(current, false)?;

            if let Some(next) = next {
                current = next;
                continue;
            }

            current = self.append_page_after(current)?;
        }
    }

    /// Allocates a new, empty page and links it after `after` in the
    /// chain, returning the new page's id.
    fn append_page_after(&mut self, after: PageId) -> Result<PageId, StorageError> {
        let (new_page_id, mut new_guard) = self.buffer_pool.new_page()?;
        SlottedPage::new(new_guard.page_mut()).init();
        drop(new_guard);
        self.buffer_pool.unpin_page(new_page_id, true)?;

        let mut link_guard = self.buffer_pool.fetch_page(after)?;
        SlottedPage::new(link_guard.page_mut()).set_next_page_id(new_page_id);
        drop(link_guard);
        self.buffer_pool.unpin_page(after, true)?;

        Ok(new_page_id)
    }

    /// Reads the tuple bytes at `rid`.
    pub fn get(&mut self, rid: Rid) -> Result<Vec<u8>, StorageError> {
        let guard = self.buffer_pool.fetch_page(rid.page_id)?;
        let bytes = slotted_read(guard.page().data(), rid.slot).map(|b| b.to_vec());
        drop(guard);
        self.buffer_pool.unpin_page(rid.page_id, false)?;
        bytes.ok_or(StorageError::CorruptPage {
            page_id: rid.page_id.0,
            reason: format!("no live tuple at slot {}", rid.slot),
        })
    }

    /// Deletes the tuple at `rid`.
    pub fn delete(&mut self, rid: Rid) -> Result<(), StorageError> {
        let mut guard = self.buffer_pool.fetch_page(rid.page_id)?;
        SlottedPage::new(guard.page_mut()).delete(rid.slot);
        drop(guard);
        self.buffer_pool.unpin_page(rid.page_id, true)?;
        Ok(())
    }

    /// Returns an iterator over every live tuple in the heap, in physical
    /// page/slot order.
    pub fn iter(&mut self) -> HeapIterator<'_, 'pool> {
        let first = self.first_page_id;
        HeapIterator { heap: self, current_page: Some(first), slot: 0 }
    }
}

/// Walks a `HeapFile` page by page, slot by slot, yielding `(Rid, Vec<u8>)`
/// pairs for every live tuple. Backs the executor's `SeqScan` operator.
pub struct HeapIterator<'a, 'pool> {
    heap: &'a mut HeapFile<'pool>,
    current_page: Option<PageId>,
    slot: u16,
}

impl Iterator for HeapIterator<'_, '_> {
    type Item = Result<(Rid, Vec<u8>), StorageError>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let page_id = self.current_page?;
            let guard = match self.heap.buffer_pool.fetch_page(page_id) {
                Ok(guard) => guard,
                Err(err) => return Some(Err(err)),
            };
            let bytes = guard.page().data();
            let count = slotted_slot_count(bytes);

            if self.slot >= count {
                let next = slotted_next_page_id(bytes);
                drop(guard);
                if let Err(err) = self.heap.buffer_pool.unpin_page(page_id, false) {
                    return Some(Err(err));
                }
                self.current_page = (next != NO_NEXT_PAGE).then_some(next);
                self.slot = 0;
                continue;
            }

            let rid = Rid::new(page_id, self.slot);
            let entry = slotted_read(bytes, self.slot).map(|b| b.to_vec());
            self.slot += 1;
            drop(guard);
            if let Err(err) = self.heap.buffer_pool.unpin_page(page_id, false) {
                return Some(Err(err));
            }
            if let Some(data) = entry {
                return Some(Ok((rid, data)));
            }
        }
    }
}
