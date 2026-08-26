use std::collections::HashSet;

use common::{PageId, Rid, TxnId};

use crate::buffer::BufferPool;
use crate::error::StorageError;
use crate::page::PageGuard;

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

pub const MAX_KEY_SIZE: usize =
    crate::page::PAGE_SIZE - HEADER_SIZE - SLOT_SIZE - KEY_LEN_PREFIX - RID_TRAILER_SIZE;

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

fn lower_bound(bytes: &[u8], key: &[u8], page_id: PageId) -> Result<u16, StorageError> {
    let count = checked_slot_count(bytes, page_id)?;
    let mut lo = 0u16;
    let mut hi = count;
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let mid_key = entry_key(entry_payload(bytes, mid, page_id)?);
        if mid_key < key { lo = mid + 1 } else { hi = mid }
    }
    Ok(lo)
}

fn upper_bound_in(payloads: &[Vec<u8>], key: &[u8]) -> usize {
    payloads.partition_point(|p| entry_key(p) <= key)
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

fn nearest_clean_boundary(entries: &[Vec<u8>], target: usize) -> usize {
    let is_clean =
        |idx: usize| idx == 0 || entry_key(&entries[idx - 1]) != entry_key(&entries[idx]);
    if is_clean(target) {
        return target;
    }
    let mut lo = target;
    let mut hi = target;
    loop {
        if lo > 0 {
            lo -= 1;
            if is_clean(lo) {
                return lo;
            }
        }
        if hi < entries.len() - 1 {
            hi += 1;
            if is_clean(hi) {
                return hi;
            }
        } else if lo == 0 {
            return 0;
        }
    }
}

fn leaf_split_point(entries: &[Vec<u8>]) -> usize {
    let target = (byte_balanced_split_point(entries) + 1).clamp(1, entries.len() - 1);
    nearest_clean_boundary(entries, target)
}

fn internal_split_point(entries: &[Vec<u8>]) -> usize {
    let mid = byte_balanced_split_point(entries);
    if entries.len() >= 3 { mid.clamp(1, entries.len() - 2) } else { mid.min(entries.len() - 1) }
}

struct Node<'a, 'pool> {
    guard: &'a mut PageGuard<'pool>,
    txn_id: TxnId,
}

impl<'a, 'pool> Node<'a, 'pool> {
    fn new(guard: &'a mut PageGuard<'pool>, txn_id: TxnId) -> Self {
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

    fn find_insert_index(&self, key: &[u8]) -> Result<u16, StorageError> {
        upper_bound(self.data(), key, self.page_id())
    }

    fn will_fit(&self, payload_len: usize) -> bool {
        let new_slots_end = HEADER_SIZE + (self.slot_count() as usize + 1) * SLOT_SIZE;
        new_slots_end + payload_len <= data_start(self.data())
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
        let mut body = current.clone();
        body[0] = if kind == NodeType::Internal { INTERNAL_TAG } else { LEAF_TAG };
        body[1..3].copy_from_slice(&(entries.len() as u16).to_le_bytes());
        let total_len: usize = entries.iter().map(Vec::len).sum();
        body[3..5].copy_from_slice(&(total_len as u16).to_le_bytes());
        body[5..9].copy_from_slice(&tail.map_or(0, |p| p.0).to_le_bytes());

        let local_header_size = HEADER_SIZE - body_start;
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
        loop {
            let guard = self.buffer_pool.fetch_page(current)?;
            let bytes = guard.page().data();
            match node_type(bytes) {
                NodeType::Leaf => {
                    drop(guard);
                    return Ok((current, path));
                }
                NodeType::Internal => {
                    let child = child_for_key(bytes, key, current)?;
                    drop(guard);
                    path.push(current);
                    current = child;
                }
            }
        }
    }

    fn leftmost_leaf(&self) -> Result<PageId, StorageError> {
        let mut current = self.root_page_id;
        loop {
            let guard = self.buffer_pool.fetch_page(current)?;
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

    fn read_node_snapshot(
        &self,
        page_id: PageId,
    ) -> Result<(Option<PageId>, Vec<Vec<u8>>), StorageError> {
        let guard = self.buffer_pool.fetch_page(page_id)?;
        let bytes = guard.page().data();
        let tail = tail_raw(bytes);
        let count = checked_slot_count(bytes, page_id)?;
        let mut entries = Vec::with_capacity(count as usize);
        for slot in 0..count {
            entries.push(entry_payload(bytes, slot, page_id)?.to_vec());
        }
        Ok((tail, entries))
    }

    pub fn get(&self, key: &[u8]) -> Result<Vec<Rid>, StorageError> {
        let (mut current, _path) = self.descend_to_leaf(key)?;
        let mut results = Vec::new();
        let mut first_leaf = true;
        loop {
            let guard = self.buffer_pool.fetch_page(current)?;
            let bytes = guard.page().data();
            let count = checked_slot_count(bytes, current)?;
            let mut idx = if first_leaf { lower_bound(bytes, key, current)? } else { 0 };
            first_leaf = false;

            let mut exhausted_matching = false;
            while idx < count {
                let payload = entry_payload(bytes, idx, current)?;
                if entry_key(payload) != key {
                    break;
                }
                results.push(leaf_rid(payload));
                idx += 1;
                exhausted_matching = idx == count;
            }
            let next = tail_raw(bytes);
            drop(guard);

            if exhausted_matching {
                if let Some(next) = next {
                    current = next;
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

    pub fn insert(&mut self, txn_id: TxnId, key: &[u8], rid: Rid) -> Result<(), StorageError> {
        if key.len() > MAX_KEY_SIZE {
            return Err(StorageError::KeyTooLarge { size: key.len(), max: MAX_KEY_SIZE });
        }

        let (leaf_page_id, mut path) = self.descend_to_leaf(key)?;
        let leaf_payload = build_leaf_payload(key, rid);

        {
            let mut guard = self.buffer_pool.fetch_page(leaf_page_id)?;
            let mut node = Node::new(&mut guard, txn_id);
            if node.will_fit(leaf_payload.len()) {
                let idx = node.find_insert_index(key)?;
                node.insert_at(idx, &leaf_payload)?;
                return Ok(());
            }
        }

        let (old_tail, mut payloads) = self.read_node_snapshot(leaf_page_id)?;
        let insert_at = upper_bound_in(&payloads, key);
        payloads.insert(insert_at, leaf_payload);
        let mid = leaf_split_point(&payloads);
        let right_payloads = payloads.split_off(mid);
        let left_payloads = payloads;

        let (right_page_id, mut right_guard) = self.buffer_pool.new_page(txn_id)?;
        Node::new(&mut right_guard, txn_id).rebuild(NodeType::Leaf, old_tail, &right_payloads)?;
        drop(right_guard);
        {
            let mut left_guard = self.buffer_pool.fetch_page(leaf_page_id)?;
            Node::new(&mut left_guard, txn_id).rebuild(
                NodeType::Leaf,
                Some(right_page_id),
                &left_payloads,
            )?;
        }

        let mut left_page_id = leaf_page_id;
        let mut pushed_key = entry_key(&right_payloads[0]).to_vec();
        let mut pushed_right = right_page_id;

        while let Some(parent_page_id) = path.pop() {
            let internal_payload = build_internal_payload(&pushed_key, pushed_right);
            {
                let mut guard = self.buffer_pool.fetch_page(parent_page_id)?;
                let mut node = Node::new(&mut guard, txn_id);
                if node.will_fit(internal_payload.len()) {
                    let idx = node.find_insert_index(&pushed_key)?;
                    node.insert_at(idx, &internal_payload)?;
                    return Ok(());
                }
            }

            let (old_tail, mut entries) = self.read_node_snapshot(parent_page_id)?;
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
            {
                let mut left_guard = self.buffer_pool.fetch_page(parent_page_id)?;
                Node::new(&mut left_guard, txn_id).rebuild(
                    NodeType::Internal,
                    Some(old_tail),
                    &left_entries,
                )?;
            }

            left_page_id = parent_page_id;
            pushed_key = new_pushed_key;
            pushed_right = new_right_page_id;
        }

        let (new_root_page_id, mut root_guard) = self.buffer_pool.new_page(txn_id)?;
        let root_entry = build_internal_payload(&pushed_key, pushed_right);
        Node::new(&mut root_guard, txn_id).rebuild(
            NodeType::Internal,
            Some(left_page_id),
            std::slice::from_ref(&root_entry),
        )?;
        self.root_page_id = new_root_page_id;
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
            let guard = self.buffer_pool.fetch_page(current).map_err(|e| e.to_string())?;
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
        let guard =
            self.buffer_pool.fetch_page(page_id).map_err(|e| format!("page {}: {e}", page_id.0))?;
        let bytes = guard.page().data();
        let count =
            checked_slot_count(bytes, page_id).map_err(|e| format!("page {}: {e}", page_id.0))?;

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
            keys.push(entry_key(payload).to_vec());
        }
        if total_payload != data_used(bytes) as usize {
            return Err(format!(
                "page {}: data_used {} does not match the sum of entry payload lengths {total_payload}",
                page_id.0,
                data_used(bytes)
            ));
        }

        let is_leaf = node_type(bytes) == NodeType::Leaf;
        for w in keys.windows(2) {
            let ordered = match w[0].cmp(&w[1]) {
                std::cmp::Ordering::Less => true,
                std::cmp::Ordering::Equal => is_leaf,
                std::cmp::Ordering::Greater => false,
            };
            if !ordered {
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

enum RangeState {
    NotStarted { start: Option<Vec<u8>> },
    InLeaf { page_id: PageId, slot: u16 },
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
                    let leaf = match start {
                        Some(key) => self.index.descend_to_leaf(key).map(|(leaf, _)| leaf),
                        None => self.index.leftmost_leaf(),
                    };
                    let leaf = match leaf {
                        Ok(leaf) => leaf,
                        Err(err) => {
                            self.state = RangeState::Done;
                            return Some(Err(err));
                        }
                    };
                    let slot = match start {
                        Some(key) => {
                            let guard = match self.index.buffer_pool.fetch_page(leaf) {
                                Ok(guard) => guard,
                                Err(err) => {
                                    self.state = RangeState::Done;
                                    return Some(Err(err));
                                }
                            };
                            match lower_bound(guard.page().data(), key, leaf) {
                                Ok(slot) => slot,
                                Err(err) => {
                                    self.state = RangeState::Done;
                                    return Some(Err(err));
                                }
                            }
                        }
                        None => 0,
                    };
                    self.state = RangeState::InLeaf { page_id: leaf, slot };
                }
                RangeState::InLeaf { page_id, slot } => {
                    let (page_id, slot) = (*page_id, *slot);
                    let guard = match self.index.buffer_pool.fetch_page(page_id) {
                        Ok(guard) => guard,
                        Err(err) => {
                            self.state = RangeState::Done;
                            return Some(Err(err));
                        }
                    };
                    let bytes = guard.page().data();
                    let count = match checked_slot_count(bytes, page_id) {
                        Ok(count) => count,
                        Err(err) => {
                            self.state = RangeState::Done;
                            return Some(Err(err));
                        }
                    };
                    if slot >= count {
                        let next = tail_raw(bytes);
                        drop(guard);
                        self.state = match next {
                            Some(next) => RangeState::InLeaf { page_id: next, slot: 0 },
                            None => RangeState::Done,
                        };
                        continue;
                    }
                    let payload = match entry_payload(bytes, slot, page_id) {
                        Ok(payload) => payload,
                        Err(err) => {
                            self.state = RangeState::Done;
                            return Some(Err(err));
                        }
                    };
                    let key = entry_key(payload).to_vec();
                    if self.end.as_ref().is_some_and(|end| key.as_slice() >= end.as_slice()) {
                        self.state = RangeState::Done;
                        return None;
                    }
                    let rid = leaf_rid(payload);
                    drop(guard);
                    self.state = RangeState::InLeaf { page_id, slot: slot + 1 };
                    return Some(Ok((key, rid)));
                }
            }
        }
    }
}
