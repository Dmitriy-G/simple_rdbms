use common::{PageId, Rid, TxnId};

use crate::buffer::BufferPool;
use crate::error::StorageError;
use crate::page::PageGuard;

const NO_NEXT_PAGE: PageId = PageId(0);

const SLOT_COUNT_RANGE: std::ops::Range<usize> = 12..14;
const DATA_USED_RANGE: std::ops::Range<usize> = 14..16;
const NEXT_PAGE_ID_RANGE: std::ops::Range<usize> = 16..20;
const HEADER_SIZE: usize = 20;
const SLOT_SIZE: usize = 4;

pub const MAX_SLOTS: u16 = ((crate::page::PAGE_SIZE - HEADER_SIZE) / SLOT_SIZE) as u16;

fn slot_offset(slot: u16) -> usize {
    HEADER_SIZE + slot as usize * SLOT_SIZE
}

fn read_u16(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn slotted_slot_count(bytes: &[u8]) -> u16 {
    read_u16(bytes, SLOT_COUNT_RANGE.start)
}

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

fn slotted_data_used(bytes: &[u8]) -> u16 {
    read_u16(bytes, DATA_USED_RANGE.start)
}

fn slotted_data_start(bytes: &[u8]) -> usize {
    crate::page::PAGE_SIZE.saturating_sub(slotted_data_used(bytes) as usize)
}

fn slotted_next_page_id(bytes: &[u8]) -> PageId {
    PageId(u32::from_le_bytes([
        bytes[NEXT_PAGE_ID_RANGE.start],
        bytes[NEXT_PAGE_ID_RANGE.start + 1],
        bytes[NEXT_PAGE_ID_RANGE.start + 2],
        bytes[NEXT_PAGE_ID_RANGE.start + 3],
    ]))
}

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

fn slotted_free_space(bytes: &[u8]) -> usize {
    let slots_end = HEADER_SIZE + slotted_slot_count(bytes) as usize * SLOT_SIZE;
    slotted_data_start(bytes).saturating_sub(slots_end)
}

pub struct SlottedPage<'a, 'pool> {
    guard: &'a mut PageGuard<'pool>,
    txn_id: TxnId,
}

impl<'a, 'pool> SlottedPage<'a, 'pool> {
    pub fn new(guard: &'a mut PageGuard<'pool>, txn_id: TxnId) -> Self {
        Self { guard, txn_id }
    }

    fn data(&self) -> &[u8; crate::page::PAGE_SIZE] {
        self.guard.page().data()
    }

    pub fn init(&mut self) -> Result<(), StorageError> {
        self.guard.write(self.txn_id, SLOT_COUNT_RANGE.start, &[0u8; 8])
    }

    pub fn slot_count(&self) -> u16 {
        slotted_slot_count(self.data())
    }

    pub fn read(&self, slot: u16) -> Result<Option<&[u8]>, StorageError> {
        slotted_read(self.data(), slot, self.guard.page_id())
    }

    pub fn free_space(&self) -> usize {
        slotted_free_space(self.data())
    }

    pub fn next_page_id(&self) -> Option<PageId> {
        let id = slotted_next_page_id(self.data());
        (id != NO_NEXT_PAGE).then_some(id)
    }

    pub fn set_next_page_id(&mut self, next: PageId) -> Result<(), StorageError> {
        debug_assert!(next != NO_NEXT_PAGE, "page 0 is the database header, never a heap page");
        self.guard.write(self.txn_id, NEXT_PAGE_ID_RANGE.start, &next.0.to_le_bytes())
    }

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

    // TODO(M5): vacuum - reclaim tombstoned tuple bytes via compaction.
    pub fn delete(&mut self, slot: u16) -> Result<(), StorageError> {
        let page_id = self.guard.page_id();
        if slotted_slot_entry(self.data(), slot, page_id)?.is_some() {
            self.guard.write(self.txn_id, slot_offset(slot) + 2, &0u16.to_le_bytes())?;
        }
        Ok(())
    }

    pub fn update_in_place(
        &mut self,
        slot: u16,
        offset: usize,
        bytes: &[u8],
    ) -> Result<(), StorageError> {
        let page_id = self.guard.page_id();
        let Some((tuple_start, tuple_len)) = slotted_slot_entry(self.data(), slot, page_id)? else {
            return Err(StorageError::CorruptPage {
                page_id: page_id.0,
                reason: format!("slot {slot} does not exist"),
            });
        };
        if tuple_len == 0 {
            return Err(StorageError::CorruptPage {
                page_id: page_id.0,
                reason: format!("slot {slot} is deleted"),
            });
        }
        if offset + bytes.len() > tuple_len as usize {
            return Err(StorageError::InPlaceUpdateOutOfBounds {
                page_id: page_id.0,
                slot,
                tuple_len: tuple_len as usize,
                offset,
                patch_len: bytes.len(),
            });
        }
        self.guard.write(self.txn_id, tuple_start as usize + offset, bytes)
    }
}

pub const MAX_TUPLE_SIZE: usize = crate::page::PAGE_SIZE - HEADER_SIZE - SLOT_SIZE;

pub struct TableHeap<'pool> {
    buffer_pool: &'pool BufferPool,
    first_page_id: PageId,
}

impl<'pool> TableHeap<'pool> {
    pub fn open(buffer_pool: &'pool BufferPool, first_page_id: PageId) -> Self {
        Self { buffer_pool, first_page_id }
    }

    pub fn create(buffer_pool: &'pool BufferPool, txn_id: TxnId) -> Result<Self, StorageError> {
        let (page_id, mut guard) = buffer_pool.new_page(txn_id)?;
        SlottedPage::new(&mut guard, txn_id).init()?;
        Ok(Self { buffer_pool, first_page_id: page_id })
    }

    pub fn first_page_id(&self) -> PageId {
        self.first_page_id
    }

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

    fn append_page_after(&mut self, txn_id: TxnId, after: PageId) -> Result<PageId, StorageError> {
        let (new_page_id, mut new_guard) = self.buffer_pool.new_page(txn_id)?;
        SlottedPage::new(&mut new_guard, txn_id).init()?;
        drop(new_guard);

        let mut link_guard = self.buffer_pool.fetch_page(after)?;
        SlottedPage::new(&mut link_guard, txn_id).set_next_page_id(new_page_id)?;

        Ok(new_page_id)
    }

    pub fn get_tuple(&self, rid: Rid) -> Result<Option<Vec<u8>>, StorageError> {
        let guard = self.buffer_pool.fetch_page(rid.page_id)?;
        let bytes = slotted_read(guard.page().data(), rid.slot, rid.page_id)?.map(|b| b.to_vec());
        Ok(bytes)
    }

    pub fn delete_tuple(&mut self, txn_id: TxnId, rid: Rid) -> Result<(), StorageError> {
        let mut guard = self.buffer_pool.fetch_page(rid.page_id)?;
        SlottedPage::new(&mut guard, txn_id).delete(rid.slot)
    }

    pub fn update_tuple_in_place(
        &mut self,
        txn_id: TxnId,
        rid: Rid,
        offset: usize,
        bytes: &[u8],
    ) -> Result<(), StorageError> {
        let mut guard = self.buffer_pool.fetch_page(rid.page_id)?;
        SlottedPage::new(&mut guard, txn_id).update_in_place(rid.slot, offset, bytes)
    }

    pub fn iter(&self) -> TableIter<'_, 'pool> {
        TableIter {
            heap: self,
            current_page: Some(self.first_page_id),
            buffered: std::collections::VecDeque::new(),
        }
    }

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

pub enum PageScan {
    Tuple { slot: u16, bytes: Vec<u8> },
    EndOfPage { next_page_id: Option<PageId> },
}

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
