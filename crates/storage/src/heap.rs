use common::{PageId, Rid, TxnId};

use crate::buffer::BufferPool;
use crate::error::StorageError;
use crate::page::PageGuard;

/// Sentinel stored in a slotted page's `next_page_id` field meaning "this is
/// the last page in the chain." Page 0 is permanently the database's own
/// header page (see `disk::DiskManager`) and so can never legitimately be a
/// heap page, which makes `PageId(0)` an unambiguous choice: it's also what
/// a page's `next_page_id` field reads as when the page is all zero, so a
/// page whose `init()` write never reached disk (e.g. a crash between
/// allocating and initializing it) still decodes as a valid, terminal,
/// empty page instead of corrupt.
const NO_NEXT_PAGE: PageId = PageId(0);

/// Byte layout of a slotted page's header, which sits just after the
/// page-wide checksum (`page::CHECKSUM_RANGE`, bytes `0..4`) and `page_lsn`
/// (`page::PAGE_LSN_RANGE`, bytes `4..12`) prefixes.
const SLOT_COUNT_RANGE: std::ops::Range<usize> = 12..14;
/// Bytes of tuple data currently used (see `slotted_data_used`).
const DATA_USED_RANGE: std::ops::Range<usize> = 14..16;
const NEXT_PAGE_ID_RANGE: std::ops::Range<usize> = 16..20;
/// The offset the slot array starts at: the checksum (4 bytes) and
/// `page_lsn` (8 bytes) prefixes followed by the slotted-page header
/// (`slot_count`, `data_used`, `next_page_id`; 8 more bytes).
const HEADER_SIZE: usize = 20;
/// Each slot is a (u16 offset, u16 len) pair.
const SLOT_SIZE: usize = 4;

/// The largest number of slots a slot array can hold without running past
/// the page: `(PAGE_SIZE - HEADER_SIZE) / SLOT_SIZE`. `SlottedPage::insert`
/// never lets `slot_count` grow past this, so a stored `slot_count` above it
/// can only come from a page that was never actually written as a slotted
/// page. With `NO_NEXT_PAGE` now `PageId(0)` (so an all-zero, uninitialized
/// page is a valid empty page rather than corrupt) and `slotted_read`
/// validating every tuple range it hands out, this is a last-resort guard
/// against misreading some *other* kind of page (e.g. the page-0 file
/// header) as a heap page - it should be unreachable in practice, not the
/// primary defense it once was.
pub const MAX_SLOTS: u16 = ((crate::page::PAGE_SIZE - HEADER_SIZE) / SLOT_SIZE) as u16;

fn slot_offset(slot: u16) -> usize {
    HEADER_SIZE + slot as usize * SLOT_SIZE
}

fn read_u16(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

/// Reads the number of slots (live or tombstoned) allocated on a slotted
/// page's raw bytes.
fn slotted_slot_count(bytes: &[u8]) -> u16 {
    read_u16(bytes, SLOT_COUNT_RANGE.start)
}

/// Reads and validates the slot count for a page being scanned from disk.
/// A `slot_count` above `MAX_SLOTS` cannot come from a real slotted page
/// (see its doc comment); this is a backstop against misreading some other
/// kind of page as a heap page, not the mechanism that makes an ordinary
/// uninitialized page safe - that comes from `NO_NEXT_PAGE` being
/// `PageId(0)`, a value a zeroed page could never falsely produce.
fn checked_slot_count(bytes: &[u8], page_id: PageId) -> Result<u16, StorageError> {
    let count = slotted_slot_count(bytes);
    if count > MAX_SLOTS {
        return Err(StorageError::CorruptPage {
            page_id: page_id.0,
            reason: format!(
                "slot count {count} exceeds the {MAX_SLOTS} slots a {}-byte page can hold",
                crate::page::PAGE_SIZE
            ),
        });
    }
    Ok(count)
}

/// Reads the number of tuple-data bytes currently used on a slotted page's
/// raw bytes. Tuple bytes are packed backward from the end of the page, so
/// this grows from `0` as tuples are inserted, rather than storing an
/// absolute offset - which is what makes an all-zero page decode as a
/// valid, empty page instead of one with no free space at all.
fn slotted_data_used(bytes: &[u8]) -> u16 {
    read_u16(bytes, DATA_USED_RANGE.start)
}

/// The offset (from the start of the page) at which tuple data currently
/// begins: `PAGE_SIZE` minus however many bytes are in use. Saturates at
/// `0` rather than underflowing: a stored `data_used` above `PAGE_SIZE`
/// cannot come from a real page (`SlottedPage::insert` never lets it grow
/// that far), only a corrupt one, and reading `0` free space for such a
/// page is the same safe "can't insert here" outcome `slotted_free_space`
/// already produces for other kinds of corruption.
fn slotted_data_start(bytes: &[u8]) -> usize {
    crate::page::PAGE_SIZE.saturating_sub(slotted_data_used(bytes) as usize)
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

/// Looks up slot `slot`'s (offset, len) entry: `Ok(None)` if `slot` is past
/// the allocated slot array. `Err(CorruptPage)` if the page's own slot
/// count is invalid (see `checked_slot_count`), checked first so every
/// entry this does return is guaranteed to sit within the page.
fn slotted_slot_entry(
    bytes: &[u8],
    slot: u16,
    page_id: PageId,
) -> Result<Option<(u16, u16)>, StorageError> {
    let count = checked_slot_count(bytes, page_id)?;
    if slot >= count {
        return Ok(None);
    }
    let at = slot_offset(slot);
    Ok(Some((read_u16(bytes, at), read_u16(bytes, at + 2))))
}

/// The raw bytes of the tuple in `slot` on a slotted page's raw bytes.
/// `Ok(None)` if the slot doesn't exist (past the allocated slot array) or
/// is tombstoned (`len == 0`). `Err(CorruptPage)` if the slot's recorded
/// `(offset, len)` doesn't describe a valid tuple - a range that runs past
/// the page, or one that overlaps the slot array itself - bytes that
/// `SlottedPage::insert` could never have produced.
fn slotted_read(bytes: &[u8], slot: u16, page_id: PageId) -> Result<Option<&[u8]>, StorageError> {
    let Some((offset, len)) = slotted_slot_entry(bytes, slot, page_id)? else {
        return Ok(None);
    };
    if len == 0 {
        return Ok(None);
    }

    let slots_end = slot_offset(checked_slot_count(bytes, page_id)?);
    let start = offset as usize;
    let end = start + len as usize;
    if start < slots_end {
        return Err(StorageError::CorruptPage {
            page_id: page_id.0,
            reason: format!(
                "slot {slot}'s tuple at offset {start} overlaps the {slots_end}-byte slot array"
            ),
        });
    }
    bytes.get(start..end).map(Some).ok_or_else(|| StorageError::CorruptPage {
        page_id: page_id.0,
        reason: format!(
            "slot {slot}'s tuple range {start}..{end} runs past the {}-byte page",
            bytes.len()
        ),
    })
}

/// The number of free bytes available for a new slot plus its tuple data.
fn slotted_free_space(bytes: &[u8]) -> usize {
    let slots_end = HEADER_SIZE + slotted_slot_count(bytes) as usize * SLOT_SIZE;
    slotted_data_start(bytes).saturating_sub(slots_end)
}

/// A view over a `PageGuard`'s bytes as a slotted page: a header, a slot
/// array growing forward from just after the header, and tuple bytes
/// packed backward from the end of the page. Slots hold an (offset, length)
/// pair so a `Rid`'s slot number stays stable even as earlier tuples on the
/// page are deleted or resized, which is what lets indexes point at a `Rid`
/// without being invalidated by unrelated writes to the same page.
///
/// Every mutating method goes through `PageGuard::write`, so every change
/// to a slotted page's bytes is logged to the write-ahead log before it is
/// applied - there is no other way to reach a page's bytes from here.
pub struct SlottedPage<'a, 'pool> {
    guard: &'a mut PageGuard<'pool>,
    txn_id: TxnId,
}

impl<'a, 'pool> SlottedPage<'a, 'pool> {
    /// Wraps `guard` for slotted-page-layout access, logging any mutation
    /// on behalf of `txn_id`.
    pub fn new(guard: &'a mut PageGuard<'pool>, txn_id: TxnId) -> Self {
        Self { guard, txn_id }
    }

    fn data(&self) -> &[u8; crate::page::PAGE_SIZE] {
        self.guard.page().data()
    }

    /// Initializes an empty page's header: no slots, no tuple data used
    /// yet, and no next page in the chain. Every field's "empty" value is
    /// `0` (`NO_NEXT_PAGE` is `PageId(0)`), so this is a no-op on a page
    /// that's already all zero - kept for explicitness, since a page that
    /// never gets this call (e.g. one whose initializing write was lost
    /// before reaching disk) is still a valid empty page, not a corrupt one.
    pub fn init(&mut self) -> Result<(), StorageError> {
        self.guard.write(self.txn_id, SLOT_COUNT_RANGE.start, &[0u8; 8])
    }

    /// The number of slots currently allocated on the page, including any
    /// occupied by deleted (tombstoned) tuples.
    pub fn slot_count(&self) -> u16 {
        slotted_slot_count(self.data())
    }

    /// The raw bytes of the tuple in `slot`, or `None` if the slot is empty
    /// or tombstoned, or `Err(CorruptPage)` if the slot's recorded range
    /// isn't a valid tuple on this page.
    pub fn read(&self, slot: u16) -> Result<Option<&[u8]>, StorageError> {
        slotted_read(self.data(), slot, self.guard.page_id())
    }

    /// The number of free bytes available for a new slot plus its tuple
    /// data.
    pub fn free_space(&self) -> usize {
        slotted_free_space(self.data())
    }

    /// The id of the next page in this heap's chain, or `None` if this is
    /// the chain's last page.
    pub fn next_page_id(&self) -> Option<PageId> {
        let id = slotted_next_page_id(self.data());
        (id != NO_NEXT_PAGE).then_some(id)
    }

    /// Links this page to `next` as the next page in the chain. `next` must
    /// never be page 0: it's the database's reserved header page, and using
    /// it as a heap page id would collide with `NO_NEXT_PAGE`.
    pub fn set_next_page_id(&mut self, next: PageId) -> Result<(), StorageError> {
        debug_assert!(next != NO_NEXT_PAGE, "page 0 is the database header, never a heap page");
        self.guard.write(self.txn_id, NEXT_PAGE_ID_RANGE.start, &next.0.to_le_bytes())
    }

    /// Appends `data` as a new tuple, returning its slot number, or `None`
    /// if the page does not have enough free space.
    pub fn insert(&mut self, data: &[u8]) -> Result<Option<u16>, StorageError> {
        let slot_count = self.slot_count();
        let new_slots_end = HEADER_SIZE + (slot_count as usize + 1) * SLOT_SIZE;
        let space_end = slotted_data_start(self.data());
        if new_slots_end + data.len() > space_end {
            return Ok(None);
        }

        let data_start = space_end - data.len();
        self.guard.write(self.txn_id, data_start, data)?;

        let mut slot_entry = [0u8; SLOT_SIZE];
        slot_entry[0..2].copy_from_slice(&(data_start as u16).to_le_bytes());
        slot_entry[2..4].copy_from_slice(&(data.len() as u16).to_le_bytes());
        self.guard.write(self.txn_id, slot_offset(slot_count), &slot_entry)?;

        let mut header = [0u8; 4];
        header[0..2].copy_from_slice(&(slot_count + 1).to_le_bytes());
        header[2..4].copy_from_slice(&((crate::page::PAGE_SIZE - data_start) as u16).to_le_bytes());
        self.guard.write(self.txn_id, SLOT_COUNT_RANGE.start, &header)?;

        Ok(Some(slot_count))
    }

    /// Tombstones `slot`, freeing its bytes for future compaction without
    /// shifting other slots' numbers.
    // TODO(M5): vacuum - reclaim tombstoned tuple bytes via compaction.
    pub fn delete(&mut self, slot: u16) -> Result<(), StorageError> {
        let page_id = self.guard.page_id();
        if slotted_slot_entry(self.data(), slot, page_id)?.is_some() {
            self.guard.write(self.txn_id, slot_offset(slot) + 2, &0u16.to_le_bytes())?;
        }
        Ok(())
    }
}

/// The largest tuple a single, otherwise-empty page can ever hold: header,
/// one slot entry, and the tuple's own bytes must all fit in `PAGE_SIZE`.
/// A tuple larger than this can never be inserted, no matter how many pages
/// are tried, so `TableHeap::insert_tuple` rejects it up front instead of
/// looping forever allocating pages for it.
pub const MAX_TUPLE_SIZE: usize = crate::page::PAGE_SIZE - HEADER_SIZE - SLOT_SIZE;

/// The on-disk heap: an unordered collection of a table's tuples, stored as
/// a singly-linked chain of slotted pages starting at `first_page_id`. This
/// is the default storage for a table before (or in place of) any index.
pub struct TableHeap<'pool> {
    buffer_pool: &'pool BufferPool,
    first_page_id: PageId,
}

impl<'pool> TableHeap<'pool> {
    /// Opens the table heap whose first page is `first_page_id`, backed by
    /// `buffer_pool`.
    pub fn open(buffer_pool: &'pool BufferPool, first_page_id: PageId) -> Self {
        Self { buffer_pool, first_page_id }
    }

    /// Creates a brand-new, empty table heap (allocates its first page) and
    /// returns a handle to it. Logs on behalf of `txn_id`.
    pub fn create(buffer_pool: &'pool BufferPool, txn_id: TxnId) -> Result<Self, StorageError> {
        let (page_id, mut guard) = buffer_pool.new_page(txn_id)?;
        SlottedPage::new(&mut guard, txn_id).init()?;
        Ok(Self { buffer_pool, first_page_id: page_id })
    }

    /// The id of this heap's first page.
    pub fn first_page_id(&self) -> PageId {
        self.first_page_id
    }

    /// Inserts `tuple_bytes` into the heap, returning the `Rid` at which it
    /// now lives. Tries the last-known page first, allocating and linking a
    /// new one if it doesn't fit there. Logs on behalf of `txn_id`.
    pub fn insert_tuple(&mut self, txn_id: TxnId, tuple_bytes: &[u8]) -> Result<Rid, StorageError> {
        if tuple_bytes.len() > MAX_TUPLE_SIZE {
            return Err(StorageError::TupleTooLarge {
                size: tuple_bytes.len(),
                max: MAX_TUPLE_SIZE,
            });
        }

        let mut current = self.first_page_id;
        loop {
            let mut guard = self.buffer_pool.fetch_page(current)?;
            let mut slotted = SlottedPage::new(&mut guard, txn_id);
            if let Some(slot) = slotted.insert(tuple_bytes)? {
                return Ok(Rid::new(current, slot));
            }
            let next = slotted.next_page_id();
            drop(guard);

            if let Some(next) = next {
                current = next;
                continue;
            }

            current = self.append_page_after(txn_id, current)?;
        }
    }

    /// Allocates a new, empty page and links it after `after` in the
    /// chain, returning the new page's id.
    fn append_page_after(&mut self, txn_id: TxnId, after: PageId) -> Result<PageId, StorageError> {
        let (new_page_id, mut new_guard) = self.buffer_pool.new_page(txn_id)?;
        SlottedPage::new(&mut new_guard, txn_id).init()?;
        drop(new_guard);

        let mut link_guard = self.buffer_pool.fetch_page(after)?;
        SlottedPage::new(&mut link_guard, txn_id).set_next_page_id(new_page_id)?;

        Ok(new_page_id)
    }

    /// Reads the tuple bytes at `rid`, or `None` if that slot holds no live
    /// tuple (deleted, or never written).
    pub fn get_tuple(&self, rid: Rid) -> Result<Option<Vec<u8>>, StorageError> {
        let guard = self.buffer_pool.fetch_page(rid.page_id)?;
        let bytes = slotted_read(guard.page().data(), rid.slot, rid.page_id)?.map(|b| b.to_vec());
        Ok(bytes)
    }

    /// Deletes the tuple at `rid`. Logs on behalf of `txn_id`.
    pub fn delete_tuple(&mut self, txn_id: TxnId, rid: Rid) -> Result<(), StorageError> {
        let mut guard = self.buffer_pool.fetch_page(rid.page_id)?;
        SlottedPage::new(&mut guard, txn_id).delete(rid.slot)
    }

    /// Returns an iterator over every live tuple in the heap, in physical
    /// page/slot order. Holds a page pinned only while reading that page's
    /// tuples out, never across the whole scan.
    pub fn iter(&self) -> TableIter<'_, 'pool> {
        TableIter {
            heap: self,
            current_page: Some(self.first_page_id),
            buffered: std::collections::VecDeque::new(),
        }
    }

    /// Scans `page_id` for the next live tuple at or after `from_slot`,
    /// fetching and releasing exactly one pin. This is the primitive a
    /// caller drives one step at a time to build its own lazy, page-at-a-
    /// time cursor over a heap (see `executor::SeqScanExecutor`) without
    /// ever holding a page pinned across its own iteration boundary — this
    /// method never holds a guard past its own return.
    pub fn scan_page(
        buffer_pool: &BufferPool,
        page_id: PageId,
        from_slot: u16,
    ) -> Result<PageScan, StorageError> {
        let guard = buffer_pool.fetch_page(page_id)?;
        let bytes = guard.page().data();
        let count = checked_slot_count(bytes, page_id)?;
        for slot in from_slot..count {
            if let Some(data) = slotted_read(bytes, slot, page_id)? {
                return Ok(PageScan::Tuple { slot, bytes: data.to_vec() });
            }
        }
        let next = slotted_next_page_id(bytes);
        Ok(PageScan::EndOfPage { next_page_id: (next != NO_NEXT_PAGE).then_some(next) })
    }
}

/// The outcome of `TableHeap::scan_page`.
pub enum PageScan {
    /// A live tuple at `slot`, with its raw bytes. The caller should resume
    /// this same page at `slot + 1` on its next call.
    Tuple {
        /// The slot the tuple was found at.
        slot: u16,
        /// The tuple's raw, still-encoded bytes.
        bytes: Vec<u8>,
    },
    /// No live tuple remains on this page at or after `from_slot`. The
    /// caller should move on to `next_page_id` (`None` at the end of the
    /// chain) and resume there at slot `0`.
    EndOfPage {
        /// The next page in the heap's chain, or `None` if this was the
        /// last one.
        next_page_id: Option<PageId>,
    },
}

/// Walks a `TableHeap` page by page, yielding `(Rid, Vec<u8>)` pairs for
/// every live tuple. Backs the executor's `SeqScan` operator. Each page is
/// fetched once, its live tuples copied out, and unpinned before the next
/// page is fetched — no page stays pinned across the whole scan.
pub struct TableIter<'a, 'pool> {
    heap: &'a TableHeap<'pool>,
    current_page: Option<PageId>,
    buffered: std::collections::VecDeque<(Rid, Vec<u8>)>,
}

impl Iterator for TableIter<'_, '_> {
    type Item = Result<(Rid, Vec<u8>), StorageError>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(entry) = self.buffered.pop_front() {
                return Some(Ok(entry));
            }

            let page_id = self.current_page?;
            let guard = match self.heap.buffer_pool.fetch_page(page_id) {
                Ok(guard) => guard,
                Err(err) => return Some(Err(err)),
            };
            let bytes = guard.page().data();
            let count = match checked_slot_count(bytes, page_id) {
                Ok(count) => count,
                Err(err) => return Some(Err(err)),
            };
            for slot in 0..count {
                match slotted_read(bytes, slot, page_id) {
                    Ok(Some(data)) => {
                        self.buffered.push_back((Rid::new(page_id, slot), data.to_vec()))
                    }
                    Ok(None) => {}
                    Err(err) => return Some(Err(err)),
                }
            }
            let next = slotted_next_page_id(bytes);
            self.current_page = (next != NO_NEXT_PAGE).then_some(next);
        }
    }
}
