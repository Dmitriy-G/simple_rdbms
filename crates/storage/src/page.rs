use std::mem::ManuallyDrop;
use std::sync::{RwLockReadGuard, RwLockWriteGuard};

use common::{FrameId, Lsn, PageId};

pub const PAGE_SIZE: usize = 4096;

pub const CHECKSUM_RANGE: std::ops::Range<usize> = 0..4;

pub const PAGE_LSN_RANGE: std::ops::Range<usize> = 4..12;

pub fn checksum_of(bytes: &[u8; PAGE_SIZE]) -> u32 {
    common::crc::crc32(&bytes[CHECKSUM_RANGE.end..])
}

pub fn stamp_checksum(bytes: &mut [u8; PAGE_SIZE]) {
    let crc = checksum_of(bytes);
    bytes[CHECKSUM_RANGE].copy_from_slice(&crc.to_le_bytes());
}

pub fn checksum_ok(bytes: &[u8; PAGE_SIZE]) -> bool {
    if bytes.iter().all(|&b| b == 0) {
        return true;
    }
    let expected = u32::from_le_bytes([
        bytes[CHECKSUM_RANGE.start],
        bytes[CHECKSUM_RANGE.start + 1],
        bytes[CHECKSUM_RANGE.start + 2],
        bytes[CHECKSUM_RANGE.start + 3],
    ]);
    expected == checksum_of(bytes)
}

#[derive(Clone)]
pub struct Page {
    id: PageId,
    data: [u8; PAGE_SIZE],
}

impl Page {
    pub fn new(id: PageId) -> Self {
        Self { id, data: [0u8; PAGE_SIZE] }
    }

    pub fn id(&self) -> PageId {
        self.id
    }

    pub fn data(&self) -> &[u8; PAGE_SIZE] {
        &self.data
    }

    pub fn data_mut(&mut self) -> &mut [u8; PAGE_SIZE] {
        &mut self.data
    }

    pub fn page_lsn(&self) -> Lsn {
        let bytes = &self.data[PAGE_LSN_RANGE];
        Lsn(u64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }

    pub fn set_page_lsn(&mut self, lsn: Lsn) {
        self.data[PAGE_LSN_RANGE].copy_from_slice(&lsn.0.to_le_bytes());
    }
}

pub struct PageReadGuard<'pool> {
    pub(crate) page_id: PageId,
    pub(crate) frame_id: FrameId,
    pub(crate) bytes: ManuallyDrop<RwLockReadGuard<'pool, Page>>,
    pub(crate) pool: &'pool crate::buffer::BufferPool,
}

impl PageReadGuard<'_> {
    pub fn page_id(&self) -> PageId {
        self.page_id
    }
}

pub struct PageWriteGuard<'pool> {
    pub(crate) page_id: PageId,
    pub(crate) frame_id: FrameId,
    pub(crate) bytes: ManuallyDrop<RwLockWriteGuard<'pool, Page>>,
    pub(crate) pool: &'pool crate::buffer::BufferPool,
}

impl PageWriteGuard<'_> {
    pub fn page_id(&self) -> PageId {
        self.page_id
    }
}
