use std::collections::HashSet;

use common::{PageId, Rid, TxnId};

use crate::buffer::BufferPool;
use crate::error::StorageError;
use crate::page::{PageReadGuard, PageWriteGuard};

const NODE_TYPE_RANGE: std::ops::Range<usize> = 12..13;
const SLOT_COUNT_RANGE: std::ops::Range<usize> = 13..15;
const DATA_USED_RANGE: std::ops::Range<usize> = 15..17;
const TAIL_RANGE: std::ops::Range<usize> = 17..21;
const HEADER_SIZE: usize = 21;
const SLOT_SIZE: usize = 4;

const LEAF_TAG: u8 = 0;
const INTERNAL_TAG: u8 = 1;

const KEY_LEN_PREFIX: usize = 2;
const RID_TRAILER_SIZE: usize = 6;
const CHILD_TRAILER_SIZE: usize = 4;

pub const MAX_KEY_SIZE: usize = crate::page::PAGE_SIZE
    - HEADER_SIZE
    - SLOT_SIZE
    - KEY_LEN_PREFIX
    - RID_TRAILER_SIZE
    - CHILD_TRAILER_SIZE;

const MAX_INTERNAL_PUSH_UP_SIZE: usize =
    KEY_LEN_PREFIX + MAX_KEY_SIZE + RID_TRAILER_SIZE + CHILD_TRAILER_SIZE;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NodeType {
    Leaf,
    Internal,
}

fn read_u16(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn read_u32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

fn node_type(bytes: &[u8]) -> NodeType {
    if bytes[NODE_TYPE_RANGE.start] == INTERNAL_TAG { NodeType::Internal } else { NodeType::Leaf }
}

fn slot_count(bytes: &[u8]) -> u16 {
    read_u16(bytes, SLOT_COUNT_RANGE.start)
}

fn data_used(bytes: &[u8]) -> u16 {
    read_u16(bytes, DATA_USED_RANGE.start)
}

fn data_start(bytes: &[u8]) -> usize {
    crate::page::PAGE_SIZE.saturating_sub(data_used(bytes) as usize)
}

fn tail_raw(bytes: &[u8]) -> Option<PageId> {
    let id = read_u32(bytes, TAIL_RANGE.start);
    (id != 0).then_some(PageId(id))
}

const MAX_SLOTS: u16 = ((crate::page::PAGE_SIZE - HEADER_SIZE) / SLOT_SIZE) as u16;

fn checked_slot_count(bytes: &[u8], page_id: PageId) -> Result<u16, StorageError> {
    let count = slot_count(bytes);
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

fn slot_offset(slot: u16) -> usize {
    HEADER_SIZE + slot as usize * SLOT_SIZE
}

fn slot_entry(bytes: &[u8], slot: u16, page_id: PageId) -> Result<(u16, u16), StorageError> {
    let count = checked_slot_count(bytes, page_id)?;
    if slot >= count {
        return Err(StorageError::CorruptPage {
            page_id: page_id.0,
            reason: format!("slot {slot} does not exist ({count} slots present)"),
        });
    }
    let at = slot_offset(slot);
    Ok((read_u16(bytes, at), read_u16(bytes, at + 2)))
}

fn entry_payload(bytes: &[u8], slot: u16, page_id: PageId) -> Result<&[u8], StorageError> {
    let (offset, len) = slot_entry(bytes, slot, page_id)?;
    let slots_end = slot_offset(checked_slot_count(bytes, page_id)?);
    let start = offset as usize;
    let end = start + len as usize;
    if start < slots_end {
        return Err(StorageError::CorruptPage {
            page_id: page_id.0,
            reason: format!(
                "slot {slot}'s entry at offset {start} overlaps the {slots_end}-byte slot array"
            ),
        });
    }
    bytes.get(start..end).ok_or_else(|| StorageError::CorruptPage {
        page_id: page_id.0,
        reason: format!(
            "slot {slot}'s entry range {start}..{end} runs past the {}-byte page",
            bytes.len()
        ),
    })
}

fn entry_key(payload: &[u8]) -> &[u8] {
    let key_len = payload.get(0..2).map_or(0, |b| u16::from_le_bytes([b[0], b[1]])) as usize;
    payload.get(KEY_LEN_PREFIX..KEY_LEN_PREFIX + key_len).unwrap_or(&[])
}

fn leaf_sort_key(payload: &[u8]) -> &[u8] {
    let key_len = payload.get(0..2).map_or(0, |b| u16::from_le_bytes([b[0], b[1]])) as usize;
    payload.get(KEY_LEN_PREFIX..KEY_LEN_PREFIX + key_len + RID_TRAILER_SIZE).unwrap_or(&[])
}

fn leaf_rid(payload: &[u8]) -> Rid {
    let key_len = read_u16(payload, 0) as usize;
    let at = KEY_LEN_PREFIX + key_len;
    Rid::new(PageId(read_u32(payload, at)), read_u16(payload, at + 4))
}

fn internal_child(payload: &[u8]) -> PageId {
    let key_len = read_u16(payload, 0) as usize;
    let at = KEY_LEN_PREFIX + key_len;
    PageId(read_u32(payload, at))
}

fn build_leaf_payload(key: &[u8], rid: Rid) -> Vec<u8> {
    let mut buf = Vec::with_capacity(KEY_LEN_PREFIX + key.len() + RID_TRAILER_SIZE);
    buf.extend_from_slice(&(key.len() as u16).to_le_bytes());
    buf.extend_from_slice(key);
    buf.extend_from_slice(&rid.page_id.0.to_le_bytes());
    buf.extend_from_slice(&rid.slot.to_le_bytes());
    buf
}

fn build_internal_payload(key: &[u8], child: PageId) -> Vec<u8> {
    let mut buf = Vec::with_capacity(KEY_LEN_PREFIX + key.len() + CHILD_TRAILER_SIZE);
    buf.extend_from_slice(&(key.len() as u16).to_le_bytes());
    buf.extend_from_slice(key);
    buf.extend_from_slice(&child.0.to_le_bytes());
    buf
}

fn upper_bound(bytes: &[u8], key: &[u8], page_id: PageId) -> Result<u16, StorageError> {
    let count = checked_slot_count(bytes, page_id)?;
    let mut lo = 0u16;
    let mut hi = count;
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let mid_key = entry_key(entry_payload(bytes, mid, page_id)?);
        if mid_key <= key { lo = mid + 1 } else { hi = mid }
    }
    Ok(lo)
}

fn upper_bound_leaf(bytes: &[u8], sort_key: &[u8], page_id: PageId) -> Result<u16, StorageError> {
    let count = checked_slot_count(bytes, page_id)?;
    let mut lo = 0u16;
    let mut hi = count;
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let mid_key = leaf_sort_key(entry_payload(bytes, mid, page_id)?);
        if mid_key <= sort_key { lo = mid + 1 } else { hi = mid }
    }
    Ok(lo)
}

fn lower_bound_leaf(bytes: &[u8], key: &[u8], page_id: PageId) -> Result<u16, StorageError> {
    let count = checked_slot_count(bytes, page_id)?;
    let mut lo = 0u16;
    let mut hi = count;
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let mid_key = leaf_sort_key(entry_payload(bytes, mid, page_id)?);
        if mid_key < key { lo = mid + 1 } else { hi = mid }
    }
    Ok(lo)
}

fn upper_bound_in(payloads: &[Vec<u8>], key: &[u8]) -> usize {
    payloads.partition_point(|p| entry_key(p) <= key)
}

fn upper_bound_in_leaf(payloads: &[Vec<u8>], sort_key: &[u8]) -> usize {
    payloads.partition_point(|p| leaf_sort_key(p) <= sort_key)
}

fn child_for_key(bytes: &[u8], key: &[u8], page_id: PageId) -> Result<PageId, StorageError> {
    let idx = upper_bound(bytes, key, page_id)?;
    if idx == 0 {
        tail_raw(bytes).ok_or_else(|| StorageError::CorruptPage {
            page_id: page_id.0,
            reason: "internal node has no tail (leftmost child) pointer".to_string(),
        })
    } else {
        Ok(internal_child(entry_payload(bytes, idx - 1, page_id)?))
    }
}

fn node_will_fit(bytes: &[u8], payload_len: usize) -> bool {
    let new_slots_end = HEADER_SIZE + (slot_count(bytes) as usize + 1) * SLOT_SIZE;
    new_slots_end + payload_len <= data_start(bytes)
}

fn read_entries(
    bytes: &[u8],
    page_id: PageId,
) -> Result<(Option<PageId>, Vec<Vec<u8>>), StorageError> {
    let tail = tail_raw(bytes);
    let count = checked_slot_count(bytes, page_id)?;
    let mut entries = Vec::with_capacity(count as usize);
    for slot in 0..count {
        entries.push(entry_payload(bytes, slot, page_id)?.to_vec());
    }
    Ok((tail, entries))
}

fn byte_balanced_split_point(entries: &[Vec<u8>]) -> usize {
    let total: usize = entries.iter().map(|p| SLOT_SIZE + p.len()).sum();
    let target = total / 2;
    let mut running = 0usize;
    for (i, entry) in entries.iter().enumerate() {
        running += SLOT_SIZE + entry.len();
        if running >= target {
            return i;
        }
    }
    entries.len() - 1
}

fn leaf_split_point(entries: &[Vec<u8>]) -> usize {
    byte_balanced_split_point(entries).saturating_add(1).clamp(1, entries.len() - 1)
}

fn internal_split_point(entries: &[Vec<u8>]) -> usize {
    let mid = byte_balanced_split_point(entries);
    if entries.len() >= 3 { mid.clamp(1, entries.len() - 2) } else { mid.min(entries.len() - 1) }
}

struct Node<'a, 'pool> {
    guard: &'a mut PageWriteGuard<'pool>,
    txn_id: TxnId,
}

impl<'a, 'pool> Node<'a, 'pool> {
    fn new(guard: &'a mut PageWriteGuard<'pool>, txn_id: TxnId) -> Self {
        Self { guard, txn_id }
    }

    fn data(&self) -> &[u8; crate::page::PAGE_SIZE] {
        self.guard.page().data()
    }

    fn page_id(&self) -> PageId {
        self.guard.page_id()
    }

    fn slot_count(&self) -> u16 {
        slot_count(self.data())
    }

    fn find_insert_index(&self, sort_key: &[u8]) -> Result<u16, StorageError> {
        let bytes = self.data();
        match node_type(bytes) {
            NodeType::Leaf => upper_bound_leaf(bytes, sort_key, self.page_id()),
            NodeType::Internal => upper_bound(bytes, sort_key, self.page_id()),
        }
    }

    fn will_fit(&self, payload_len: usize) -> bool {
        node_will_fit(self.data(), payload_len)
    }

    fn init(&mut self, kind: NodeType, tail: Option<PageId>) -> Result<(), StorageError> {
        let mut header = [0u8; HEADER_SIZE - NODE_TYPE_RANGE.start];
        header[0] = if kind == NodeType::Internal { INTERNAL_TAG } else { LEAF_TAG };
        header[5..9].copy_from_slice(&tail.map_or(0, |p| p.0).to_le_bytes());
        self.guard.write(self.txn_id, NODE_TYPE_RANGE.start, &header)
    }

    fn insert_at(&mut self, index: u16, payload: &[u8]) -> Result<(), StorageError> {
        let count = self.slot_count();
        let data_at = data_start(self.data()) - payload.len();
        self.guard.write(self.txn_id, data_at, payload)?;

        if index < count {
            let shifted = self.data()[slot_offset(index)..slot_offset(count)].to_vec();
            self.guard.write(self.txn_id, slot_offset(index + 1), &shifted)?;
        }

        let mut slot_entry = [0u8; SLOT_SIZE];
        slot_entry[0..2].copy_from_slice(&(data_at as u16).to_le_bytes());
        slot_entry[2..4].copy_from_slice(&(payload.len() as u16).to_le_bytes());
        self.guard.write(self.txn_id, slot_offset(index), &slot_entry)?;

        let mut header = [0u8; 4];
        header[0..2].copy_from_slice(&(count + 1).to_le_bytes());
        header[2..4]
            .copy_from_slice(&(data_used(self.data()) + payload.len() as u16).to_le_bytes());
        self.guard.write(self.txn_id, SLOT_COUNT_RANGE.start, &header)
    }

    fn rebuild(
        &mut self,
        kind: NodeType,
        tail: Option<PageId>,
        entries: &[Vec<u8>],
    ) -> Result<(), StorageError> {
        let body_start = NODE_TYPE_RANGE.start;
        let current = self.data()[body_start..].to_vec();
        let local_header_size = HEADER_SIZE - body_start;
        let total_len: usize = entries.iter().map(Vec::len).sum();
        let required = local_header_size + entries.len() * SLOT_SIZE + total_len;
        if required > current.len() {
            return Err(StorageError::NodeOverflow {
                page_id: self.page_id().0,
                required,
                capacity: current.len(),
            });
        }

        let mut body = current.clone();
        body[0] = if kind == NodeType::Internal { INTERNAL_TAG } else { LEAF_TAG };
        body[1..3].copy_from_slice(&(entries.len() as u16).to_le_bytes());
        body[3..5].copy_from_slice(&(total_len as u16).to_le_bytes());
        body[5..9].copy_from_slice(&tail.map_or(0, |p| p.0).to_le_bytes());

        let mut data_end = body.len();
        for (i, entry) in entries.iter().enumerate() {
            data_end -= entry.len();
            body[data_end..data_end + entry.len()].copy_from_slice(entry);
            let slot_at = local_header_size + i * SLOT_SIZE;
            let absolute_offset = (data_end + body_start) as u16;
            body[slot_at..slot_at + 2].copy_from_slice(&absolute_offset.to_le_bytes());
            body[slot_at + 2..slot_at + 4].copy_from_slice(&(entry.len() as u16).to_le_bytes());
        }

        for range in changed_byte_runs(&current, &body) {
            self.guard.write(self.txn_id, body_start + range.start, &body[range])?;
        }
        Ok(())
    }
}

const REBUILD_MERGE_GAP: usize = 16;

fn changed_byte_runs(old: &[u8], new: &[u8]) -> Vec<std::ops::Range<usize>> {
    let mut runs: Vec<std::ops::Range<usize>> = Vec::new();
    let mut i = 0;
    while i < old.len() {
        if old[i] == new[i] {
            i += 1;
            continue;
        }
        let start = i;
        while i < old.len() && old[i] != new[i] {
            i += 1;
        }
        match runs.last_mut() {
            Some(last) if start - last.end < REBUILD_MERGE_GAP => last.end = i,
            _ => runs.push(start..i),
        }
    }
    runs
}

pub struct BTreeIndex<'pool> {
    buffer_pool: &'pool BufferPool,
    root_page_id: PageId,
}

impl<'pool> BTreeIndex<'pool> {
    pub fn create(buffer_pool: &'pool BufferPool, txn_id: TxnId) -> Result<Self, StorageError> {
        let (page_id, mut guard) = buffer_pool.new_page(txn_id)?;
        Node::new(&mut guard, txn_id).init(NodeType::Leaf, None)?;
        Ok(Self { buffer_pool, root_page_id: page_id })
    }

    pub fn open(buffer_pool: &'pool BufferPool, root_page_id: PageId) -> Self {
        Self { buffer_pool, root_page_id }
    }

    pub fn root_page_id(&self) -> PageId {
        self.root_page_id
    }

    fn descend_to_leaf(&self, key: &[u8]) -> Result<(PageId, Vec<PageId>), StorageError> {
        let mut path = Vec::new();
        let mut current = self.root_page_id;
        let mut parent_guard: Option<PageReadGuard<'pool>> = None;
        loop {
            let guard = self.buffer_pool.fetch_page_read(current)?;
            drop(parent_guard.take());
            let bytes = guard.page().data();
            match node_type(bytes) {
                NodeType::Leaf => {
                    return Ok((current, path));
                }
                NodeType::Internal => {
                    let child = child_for_key(bytes, key, current)?;
                    path.push(current);
                    current = child;
                    parent_guard = Some(guard);
                }
            }
        }
    }

    fn leftmost_leaf(&self) -> Result<PageId, StorageError> {
        let mut current = self.root_page_id;
        loop {
            let guard = self.buffer_pool.fetch_page_read(current)?;
            let bytes = guard.page().data();
            match node_type(bytes) {
                NodeType::Leaf => {
                    drop(guard);
                    return Ok(current);
                }
                NodeType::Internal => {
                    let child = tail_raw(bytes).ok_or_else(|| StorageError::CorruptPage {
                        page_id: current.0,
                        reason: "internal node has no tail (leftmost child) pointer".to_string(),
                    })?;
                    drop(guard);
                    current = child;
                }
            }
        }
    }

    fn descend_for_insert(
        &self,
        sort_key: &[u8],
        leaf_payload_len: usize,
    ) -> Result<(Vec<PageWriteGuard<'pool>>, PageWriteGuard<'pool>), StorageError> {
        let mut ancestors: Vec<PageWriteGuard<'pool>> = Vec::new();
        let mut current = self.root_page_id;
        loop {
            let guard = self.buffer_pool.fetch_page(current)?;
            let bytes = guard.page().data();
            match node_type(bytes) {
                NodeType::Leaf => {
                    if node_will_fit(bytes, leaf_payload_len) {
                        ancestors.clear();
                    }
                    return Ok((ancestors, guard));
                }
                NodeType::Internal => {
                    let child = child_for_key(bytes, sort_key, current)?;
                    if node_will_fit(bytes, MAX_INTERNAL_PUSH_UP_SIZE) {
                        ancestors.clear();
                    }
                    ancestors.push(guard);
                    current = child;
                }
            }
        }
    }

    pub fn get(&self, key: &[u8]) -> Result<Vec<Rid>, StorageError> {
        let (mut current, _path) = self.descend_to_leaf(key)?;
        let mut results = Vec::new();
        let mut first_leaf = true;
        let mut guard = self.buffer_pool.fetch_page_read(current)?;
        loop {
            let bytes = guard.page().data();
            let count = checked_slot_count(bytes, current)?;
            let mut idx = if first_leaf { lower_bound_leaf(bytes, key, current)? } else { 0 };
            first_leaf = false;

            while idx < count {
                let payload = entry_payload(bytes, idx, current)?;
                if entry_key(payload) != key {
                    break;
                }
                results.push(leaf_rid(payload));
                idx += 1;
            }
            let ran_off_the_end = idx == count;
            let next = tail_raw(bytes);

            if ran_off_the_end {
                if let Some(next_page_id) = next {
                    let next_guard = self.buffer_pool.fetch_page_read(next_page_id)?;
                    drop(guard);
                    guard = next_guard;
                    current = next_page_id;
                    continue;
                }
            }
            break;
        }
        Ok(results)
    }

    pub fn range_scan(
        &self,
        start: Option<&[u8]>,
        end: Option<&[u8]>,
    ) -> BTreeRangeIterator<'_, 'pool> {
        BTreeRangeIterator {
            index: self,
            end: end.map(<[u8]>::to_vec),
            state: RangeState::NotStarted { start: start.map(<[u8]>::to_vec) },
        }
    }

    pub fn leaf_for_start(&self, start: Option<&[u8]>) -> Result<PageId, StorageError> {
        match start {
            Some(key) => {
                let (leaf, _path) = self.descend_to_leaf(key)?;
                Ok(leaf)
            }
            None => self.leftmost_leaf(),
        }
    }

    pub fn scan_leaf(
        buffer_pool: &BufferPool,
        leaf_page_id: PageId,
        after: Option<&[u8]>,
    ) -> Result<LeafScan, StorageError> {
        let guard = buffer_pool.fetch_page_read(leaf_page_id)?;
        let bytes = guard.page().data();
        let count = checked_slot_count(bytes, leaf_page_id)?;
        let mut slot = match after {
            Some(sort_key) => lower_bound_leaf(bytes, sort_key, leaf_page_id)?,
            None => 0,
        };
        if let Some(sort_key) = after {
            if slot < count && leaf_sort_key(entry_payload(bytes, slot, leaf_page_id)?) == sort_key
            {
                slot += 1;
            }
        }
        if slot >= count {
            return Ok(LeafScan::EndOfLeaf { next_leaf_page_id: tail_raw(bytes) });
        }
        let payload = entry_payload(bytes, slot, leaf_page_id)?;
        Ok(LeafScan::Entry {
            slot,
            key: entry_key(payload).to_vec(),
            sort_key: leaf_sort_key(payload).to_vec(),
            rid: leaf_rid(payload),
        })
    }

    pub fn insert(&mut self, txn_id: TxnId, key: &[u8], rid: Rid) -> Result<(), StorageError> {
        if key.len() > MAX_KEY_SIZE {
            return Err(StorageError::KeyTooLarge { size: key.len(), max: MAX_KEY_SIZE });
        }

        let leaf_payload = build_leaf_payload(key, rid);
        let sort_key = leaf_sort_key(&leaf_payload).to_vec();
        let (mut ancestors, mut leaf_guard) =
            self.descend_for_insert(&sort_key, leaf_payload.len())?;

        {
            let mut node = Node::new(&mut leaf_guard, txn_id);
            if node.will_fit(leaf_payload.len()) {
                let idx = node.find_insert_index(&sort_key)?;
                node.insert_at(idx, &leaf_payload)?;
                return Ok(());
            }
        }

        let leaf_page_id = leaf_guard.page_id();
        let (old_tail, mut payloads) = read_entries(leaf_guard.page().data(), leaf_page_id)?;
        let insert_at = upper_bound_in_leaf(&payloads, &sort_key);
        payloads.insert(insert_at, leaf_payload);
        let mid = leaf_split_point(&payloads);
        let right_payloads = payloads.split_off(mid);
        let left_payloads = payloads;

        let (right_page_id, mut right_guard) = self.buffer_pool.new_page(txn_id)?;
        Node::new(&mut right_guard, txn_id).rebuild(NodeType::Leaf, old_tail, &right_payloads)?;
        drop(right_guard);
        Node::new(&mut leaf_guard, txn_id).rebuild(
            NodeType::Leaf,
            Some(right_page_id),
            &left_payloads,
        )?;

        let mut left_page_id = leaf_page_id;
        let mut pushed_key = leaf_sort_key(&right_payloads[0]).to_vec();
        let mut pushed_right = right_page_id;
        let mut top_guard = leaf_guard;

        while let Some(mut parent_guard) = ancestors.pop() {
            let internal_payload = build_internal_payload(&pushed_key, pushed_right);
            {
                let mut node = Node::new(&mut parent_guard, txn_id);
                if node.will_fit(internal_payload.len()) {
                    let idx = node.find_insert_index(&pushed_key)?;
                    node.insert_at(idx, &internal_payload)?;
                    return Ok(());
                }
            }

            let parent_page_id = parent_guard.page_id();
            let (old_tail, mut entries) = read_entries(parent_guard.page().data(), parent_page_id)?;
            let old_tail = old_tail.ok_or_else(|| StorageError::CorruptPage {
                page_id: parent_page_id.0,
                reason: "internal node has no tail (leftmost child) pointer".to_string(),
            })?;
            let insert_at = upper_bound_in(&entries, &pushed_key);
            entries.insert(insert_at, internal_payload);

            let split_at = internal_split_point(&entries);
            let pushed_entry = entries.remove(split_at);
            let right_entries = entries.split_off(split_at);
            let left_entries = entries;

            let new_pushed_key = entry_key(&pushed_entry).to_vec();
            let new_pushed_tail = internal_child(&pushed_entry);

            let (new_right_page_id, mut new_right_guard) = self.buffer_pool.new_page(txn_id)?;
            Node::new(&mut new_right_guard, txn_id).rebuild(
                NodeType::Internal,
                Some(new_pushed_tail),
                &right_entries,
            )?;
            drop(new_right_guard);
            Node::new(&mut parent_guard, txn_id).rebuild(
                NodeType::Internal,
                Some(old_tail),
                &left_entries,
            )?;

            left_page_id = parent_page_id;
            pushed_key = new_pushed_key;
            pushed_right = new_right_page_id;
            top_guard = parent_guard;
        }

        let (new_root_page_id, mut root_guard) = self.buffer_pool.new_page(txn_id)?;
        let root_entry = build_internal_payload(&pushed_key, pushed_right);
        Node::new(&mut root_guard, txn_id).rebuild(
            NodeType::Internal,
            Some(left_page_id),
            std::slice::from_ref(&root_entry),
        )?;
        self.root_page_id = new_root_page_id;
        drop(top_guard);
        Ok(())
    }

    pub fn delete(&mut self, key: &[u8]) -> Result<(), StorageError> {
        let _ = key;
        todo!("descend to the target leaf, remove the entry, rebalance on underflow")
    }

    pub fn check_invariants(&self, key_type: Option<types::DataType>) -> Result<(), String> {
        let mut state =
            InvariantState { visited: HashSet::new(), leaf_depth: None, leaves: Vec::new() };
        self.check_node(self.root_page_id, (None, None), 0, key_type, &mut state)?;

        let mut via_tail = Vec::new();
        let mut current = self.leftmost_leaf().map_err(|e| e.to_string())?;
        loop {
            via_tail.push(current);
            let guard = self.buffer_pool.fetch_page_read(current).map_err(|e| e.to_string())?;
            let next = tail_raw(guard.page().data());
            drop(guard);
            match next {
                Some(next) => current = next,
                None => break,
            }
        }
        if via_tail != state.leaves {
            return Err(format!(
                "leaf sibling chain {via_tail:?} does not match key-order leaf traversal {:?}",
                state.leaves
            ));
        }
        Ok(())
    }

    fn check_node(
        &self,
        page_id: PageId,
        bounds: (Option<&[u8]>, Option<&[u8]>),
        depth: usize,
        key_type: Option<types::DataType>,
        state: &mut InvariantState,
    ) -> Result<(), String> {
        if !state.visited.insert(page_id) {
            return Err(format!("page {} is reachable from more than one parent", page_id.0));
        }

        let (low, high) = bounds;
        let guard = self
            .buffer_pool
            .fetch_page_read(page_id)
            .map_err(|e| format!("page {}: {e}", page_id.0))?;
        let bytes = guard.page().data();
        let count =
            checked_slot_count(bytes, page_id).map_err(|e| format!("page {}: {e}", page_id.0))?;

        let is_leaf = node_type(bytes) == NodeType::Leaf;
        let slots_end = slot_offset(count);
        let mut total_payload = 0usize;
        let mut keys: Vec<Vec<u8>> = Vec::with_capacity(count as usize);
        for i in 0..count {
            let (offset, len) =
                slot_entry(bytes, i, page_id).map_err(|e| format!("page {}: {e}", page_id.0))?;
            if (offset as usize) < slots_end {
                return Err(format!(
                    "page {}: slot {i}'s entry at offset {offset} overlaps the {slots_end}-byte \
                     slot array",
                    page_id.0
                ));
            }
            total_payload += len as usize;
            let payload =
                entry_payload(bytes, i, page_id).map_err(|e| format!("page {}: {e}", page_id.0))?;
            keys.push(if is_leaf { leaf_sort_key(payload) } else { entry_key(payload) }.to_vec());
        }
        if total_payload != data_used(bytes) as usize {
            return Err(format!(
                "page {}: data_used {} does not match the sum of entry payload lengths {total_payload}",
                page_id.0,
                data_used(bytes)
            ));
        }

        for w in keys.windows(2) {
            if w[0].cmp(&w[1]) != std::cmp::Ordering::Less {
                return Err(format!(
                    "page {}: keys are not strictly increasing bytewise ({} then {})",
                    page_id.0,
                    render_key(&w[0], key_type),
                    render_key(&w[1], key_type)
                ));
            }
        }
        for key in &keys {
            if low.is_some_and(|low| key.as_slice() < low) {
                return Err(format!(
                    "page {}: key {} is below its inherited lower bound",
                    page_id.0,
                    render_key(key, key_type)
                ));
            }
            if high.is_some_and(|high| key.as_slice() >= high) {
                return Err(format!(
                    "page {}: key {} is not below its inherited upper bound",
                    page_id.0,
                    render_key(key, key_type)
                ));
            }
        }

        if is_leaf {
            match state.leaf_depth {
                None => state.leaf_depth = Some(depth),
                Some(expected) if expected == depth => {}
                Some(expected) => {
                    return Err(format!(
                        "page {}: leaf is at depth {depth}, but another leaf is at depth {expected}",
                        page_id.0
                    ));
                }
            }
            state.leaves.push(page_id);
            return Ok(());
        }

        let mut children = Vec::with_capacity(count as usize + 1);
        children.push(tail_raw(bytes).ok_or_else(|| {
            format!("page {}: internal node has no tail (leftmost child) pointer", page_id.0)
        })?);
        for i in 0..count {
            let payload =
                entry_payload(bytes, i, page_id).map_err(|e| format!("page {}: {e}", page_id.0))?;
            children.push(internal_child(payload));
        }
        drop(guard);

        for (i, &child) in children.iter().enumerate() {
            let child_low = if i == 0 { low } else { Some(keys[i - 1].as_slice()) };
            let child_high = if i == count as usize { high } else { Some(keys[i].as_slice()) };
            self.check_node(child, (child_low, child_high), depth + 1, key_type, state)?;
        }
        Ok(())
    }
}

fn render_key(key: &[u8], key_type: Option<types::DataType>) -> String {
    if let Some(data_type) = key_type {
        if let Ok((value, consumed)) = types::decode_memcomparable(key, data_type) {
            if consumed == key.len() {
                return format!("{value:?}");
            }
        }
    }
    format!("{key:?}")
}

struct InvariantState {
    visited: HashSet<PageId>,
    leaf_depth: Option<usize>,
    leaves: Vec<PageId>,
}

pub enum LeafScan {
    Entry { slot: u16, key: Vec<u8>, sort_key: Vec<u8>, rid: Rid },
    EndOfLeaf { next_leaf_page_id: Option<PageId> },
}

enum RangeState {
    NotStarted { start: Option<Vec<u8>> },
    InLeaf { page_id: PageId, after: Option<Vec<u8>> },
    Done,
}

pub struct BTreeRangeIterator<'a, 'pool> {
    index: &'a BTreeIndex<'pool>,
    end: Option<Vec<u8>>,
    state: RangeState,
}

impl Iterator for BTreeRangeIterator<'_, '_> {
    type Item = Result<(Vec<u8>, Rid), StorageError>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            match &self.state {
                RangeState::Done => return None,
                RangeState::NotStarted { start } => {
                    let page_id = match self.index.leaf_for_start(start.as_deref()) {
                        Ok(page_id) => page_id,
                        Err(err) => {
                            self.state = RangeState::Done;
                            return Some(Err(err));
                        }
                    };
                    self.state = RangeState::InLeaf { page_id, after: start.clone() };
                }
                RangeState::InLeaf { page_id, after } => {
                    let page_id = *page_id;
                    let after = after.clone();
                    let scan = match BTreeIndex::scan_leaf(
                        self.index.buffer_pool,
                        page_id,
                        after.as_deref(),
                    ) {
                        Ok(scan) => scan,
                        Err(err) => {
                            self.state = RangeState::Done;
                            return Some(Err(err));
                        }
                    };
                    match scan {
                        LeafScan::EndOfLeaf { next_leaf_page_id } => {
                            self.state = match next_leaf_page_id {
                                Some(next) => RangeState::InLeaf { page_id: next, after },
                                None => RangeState::Done,
                            };
                        }
                        LeafScan::Entry { key, sort_key, rid, .. } => {
                            if self.end.as_ref().is_some_and(|end| key.as_slice() >= end.as_slice())
                            {
                                self.state = RangeState::Done;
                                return None;
                            }
                            self.state = RangeState::InLeaf { page_id, after: Some(sort_key) };
                            return Some(Ok((key, rid)));
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn page_bytes_from_payloads(payloads: &[Vec<u8>]) -> Vec<u8> {
        let mut buf = vec![0u8; crate::page::PAGE_SIZE];
        buf[NODE_TYPE_RANGE.start] = LEAF_TAG;
        buf[SLOT_COUNT_RANGE].copy_from_slice(&(payloads.len() as u16).to_le_bytes());
        let mut end = crate::page::PAGE_SIZE;
        for (i, payload) in payloads.iter().enumerate() {
            end -= payload.len();
            buf[end..end + payload.len()].copy_from_slice(payload);
            let slot_at = slot_offset(i as u16);
            buf[slot_at..slot_at + 2].copy_from_slice(&(end as u16).to_le_bytes());
            buf[slot_at + 2..slot_at + 4].copy_from_slice(&(payload.len() as u16).to_le_bytes());
        }
        buf[DATA_USED_RANGE]
            .copy_from_slice(&((crate::page::PAGE_SIZE - end) as u16).to_le_bytes());
        buf
    }

    fn uniform_entries(count: usize, len: usize) -> Vec<Vec<u8>> {
        vec![vec![0u8; len]; count]
    }

    #[test]
    fn leaf_split_point_stays_in_bounds_for_the_duplicate_key_workload_that_once_corrupted_the_tree()
     {
        let entries = uniform_entries(240, 13);
        let split = leaf_split_point(&entries);
        assert!(split > 0 && split < entries.len(), "split {split} out of bounds");
    }

    #[test]
    fn internal_split_point_stays_in_bounds_for_the_duplicate_key_workload_that_once_corrupted_the_tree()
     {
        let entries = uniform_entries(240, 13);
        let split = internal_split_point(&entries);
        assert!(split > 0 && split < entries.len(), "split {split} out of bounds");
    }

    proptest! {
        #[test]
        fn byte_balanced_split_point_always_indexes_within_entries(
            lens in proptest::collection::vec(1usize..200, 1..300)
        ) {
            let entries: Vec<Vec<u8>> = lens.into_iter().map(|len| vec![0u8; len]).collect();
            let split = byte_balanced_split_point(&entries);
            prop_assert!(split < entries.len());
        }

        #[test]
        fn leaf_split_point_produces_two_nonempty_halves(
            lens in proptest::collection::vec(1usize..200, 2..300)
        ) {
            let entries: Vec<Vec<u8>> = lens.into_iter().map(|len| vec![0u8; len]).collect();
            let split = leaf_split_point(&entries);
            prop_assert!(split >= 1);
            prop_assert!(split < entries.len());
        }

        #[test]
        fn internal_split_point_leaves_a_key_on_both_sides(
            lens in proptest::collection::vec(1usize..200, 3..300)
        ) {
            let entries: Vec<Vec<u8>> = lens.into_iter().map(|len| vec![0u8; len]).collect();
            let split = internal_split_point(&entries);
            prop_assert!(split >= 1);
            prop_assert!(split <= entries.len() - 2);
        }
    }

    #[test]
    fn upper_bound_in_returns_the_count_when_every_entry_shares_the_search_key() {
        let key: &[u8] = b"k";
        let entries: Vec<Vec<u8>> =
            (0..16).map(|_| build_internal_payload(key, PageId(1))).collect();
        assert_eq!(upper_bound_in(&entries, key), entries.len());
    }

    #[test]
    fn upper_bound_in_skips_every_duplicate_and_lands_on_the_first_greater_key() {
        let mut entries: Vec<Vec<u8>> =
            (0..8).map(|_| build_internal_payload(b"a", PageId(1))).collect();
        entries.push(build_internal_payload(b"b", PageId(2)));
        assert_eq!(upper_bound_in(&entries, b"a"), 8);
    }

    #[test]
    fn upper_bound_in_leaf_returns_the_count_when_every_entry_shares_the_sort_key() {
        let payload = build_leaf_payload(b"k", Rid::new(PageId(1), 0));
        let sort_key = leaf_sort_key(&payload).to_vec();
        let entries: Vec<Vec<u8>> = (0..16).map(|_| payload.clone()).collect();
        assert_eq!(upper_bound_in_leaf(&entries, &sort_key), entries.len());
    }

    #[test]
    fn upper_bound_on_a_page_returns_the_slot_count_when_every_key_is_a_duplicate() {
        let payloads: Vec<Vec<u8>> =
            (0..10).map(|_| build_internal_payload(b"dup", PageId(1))).collect();
        let bytes = page_bytes_from_payloads(&payloads);
        let count = upper_bound(&bytes, b"dup", PageId(0)).expect("well-formed page");
        assert_eq!(count as usize, payloads.len());
    }

    #[test]
    fn lower_bound_leaf_finds_the_first_of_a_run_of_duplicate_sort_keys() {
        let dup_payload = build_leaf_payload(b"dup", Rid::new(PageId(1), 0));
        let sort_key = leaf_sort_key(&dup_payload).to_vec();
        let mut payloads = vec![build_leaf_payload(b"aaa", Rid::new(PageId(1), 0))];
        payloads.extend((0..5).map(|_| dup_payload.clone()));
        let bytes = page_bytes_from_payloads(&payloads);
        let idx = lower_bound_leaf(&bytes, &sort_key, PageId(0)).expect("well-formed page");
        assert_eq!(idx, 1);
    }

    #[test]
    fn upper_bound_leaf_finds_one_past_the_last_of_a_run_of_duplicate_sort_keys() {
        let dup_payload = build_leaf_payload(b"dup", Rid::new(PageId(1), 0));
        let sort_key = leaf_sort_key(&dup_payload).to_vec();
        let mut payloads: Vec<Vec<u8>> = (0..5).map(|_| dup_payload.clone()).collect();
        payloads.push(build_leaf_payload(b"zzz", Rid::new(PageId(1), 0)));
        let bytes = page_bytes_from_payloads(&payloads);
        let idx = upper_bound_leaf(&bytes, &sort_key, PageId(0)).expect("well-formed page");
        assert_eq!(idx, 5);
    }
}
