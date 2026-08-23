use common::{PageId, Rid};

use crate::buffer::BufferPool;
use crate::error::StorageError;

pub struct BTreeIndex<'pool> {
    #[allow(dead_code)]
    buffer_pool: &'pool mut BufferPool,
    #[allow(dead_code)]
    root_page_id: PageId,
}

impl<'pool> BTreeIndex<'pool> {
    pub fn create(buffer_pool: &'pool mut BufferPool) -> Result<Self, StorageError> {
        let _ = &buffer_pool;
        todo!("allocate a root page, initialize it as an empty leaf node")
    }

    pub fn open(buffer_pool: &'pool mut BufferPool, root_page_id: PageId) -> Self {
        Self { buffer_pool, root_page_id }
    }

    pub fn insert(&mut self, key: &[u8], rid: Rid) -> Result<(), StorageError> {
        let _ = (key, rid);
        todo!("descend to the target leaf, insert the entry, split and propagate on overflow")
    }

    pub fn delete(&mut self, key: &[u8]) -> Result<(), StorageError> {
        let _ = key;
        todo!("descend to the target leaf, remove the entry, rebalance on underflow")
    }

    pub fn get(&mut self, key: &[u8]) -> Result<Vec<Rid>, StorageError> {
        let _ = key;
        todo!("descend to the target leaf and collect matching entries")
    }

    pub fn range_scan(
        &mut self,
        start: Option<&[u8]>,
        end: Option<&[u8]>,
    ) -> BTreeRangeIterator<'_, 'pool> {
        let _ = (start, end);
        BTreeRangeIterator { index: self }
    }
}

pub struct BTreeRangeIterator<'a, 'pool> {
    #[allow(dead_code)]
    index: &'a mut BTreeIndex<'pool>,
}

impl Iterator for BTreeRangeIterator<'_, '_> {
    type Item = Result<(Vec<u8>, Rid), StorageError>;

    fn next(&mut self) -> Option<Self::Item> {
        todo!("yield the current leaf entry, then advance the cursor or leaf sibling")
    }
}
