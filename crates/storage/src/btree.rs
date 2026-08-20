use common::{PageId, Rid};

use crate::buffer::BufferPool;
use crate::error::StorageError;

/// A disk-resident B+tree index: internal nodes hold separator keys and
/// child `PageId`s, leaf nodes hold keys and the `Rid`s of the matching
/// heap tuples, and leaves are linked for fast ordered range scans. Keys
/// are stored as their already-encoded `types::Encode` bytes so the index
/// stays agnostic to which column(s) it indexes.
pub struct BTreeIndex<'pool> {
    #[allow(dead_code)]
    buffer_pool: &'pool mut BufferPool,
    #[allow(dead_code)]
    root_page_id: PageId,
}

impl<'pool> BTreeIndex<'pool> {
    /// Creates a brand-new, empty index (a single empty leaf as root).
    pub fn create(buffer_pool: &'pool mut BufferPool) -> Result<Self, StorageError> {
        let _ = &buffer_pool;
        todo!("allocate a root page, initialize it as an empty leaf node")
    }

    /// Opens an existing index whose root is `root_page_id`.
    pub fn open(buffer_pool: &'pool mut BufferPool, root_page_id: PageId) -> Self {
        Self { buffer_pool, root_page_id }
    }

    /// Inserts `key` mapping to `rid`, splitting nodes on overflow and
    /// growing the tree's height if the root splits.
    pub fn insert(&mut self, key: &[u8], rid: Rid) -> Result<(), StorageError> {
        let _ = (key, rid);
        todo!("descend to the target leaf, insert the entry, split and propagate on overflow")
    }

    /// Removes the entry for `key`, merging or redistributing underflowing
    /// nodes.
    pub fn delete(&mut self, key: &[u8]) -> Result<(), StorageError> {
        let _ = key;
        todo!("descend to the target leaf, remove the entry, rebalance on underflow")
    }

    /// Looks up the `Rid`s stored under `key` (a B+tree index is not
    /// required to enforce uniqueness).
    pub fn get(&mut self, key: &[u8]) -> Result<Vec<Rid>, StorageError> {
        let _ = key;
        todo!("descend to the target leaf and collect matching entries")
    }

    /// Returns an iterator over entries with keys in `[start, end)`,
    /// following leaf sibling pointers. `start: None` means unbounded
    /// below; `end: None` means unbounded above.
    pub fn range_scan(
        &mut self,
        start: Option<&[u8]>,
        end: Option<&[u8]>,
    ) -> BTreeRangeIterator<'_, 'pool> {
        let _ = (start, end);
        BTreeRangeIterator { index: self }
    }
}

/// Walks matching leaf entries of a `BTreeIndex` in key order, crossing
/// leaf pages via their sibling pointers. Backs the executor's index-scan
/// operator.
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
