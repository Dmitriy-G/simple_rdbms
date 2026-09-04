use common::{IndexId, PageId, TableId};
use storage::btree::{BTreeIndex, LeafScan};
use storage::heap::TableHeap;
use txn::LockMode;
use types::{DataType, Tuple};

use crate::context::ExecutorContext;
use crate::error::ExecutorError;
use crate::executor::Executor;

pub struct IndexScanExecutor {
    index_id: IndexId,
    table_id: TableId,
    start: Option<Vec<u8>>,
    end: Option<Vec<u8>>,
    column_types: Vec<DataType>,
    table_first_page_id: Option<PageId>,
    current_leaf: Option<PageId>,
    next_after: Option<Vec<u8>>,
}

impl IndexScanExecutor {
    pub fn new(
        index_id: IndexId,
        table_id: TableId,
        start: Option<Vec<u8>>,
        end: Option<Vec<u8>>,
    ) -> Self {
        Self {
            index_id,
            table_id,
            start,
            end,
            column_types: Vec::new(),
            table_first_page_id: None,
            current_leaf: None,
            next_after: None,
        }
    }
}

impl Executor for IndexScanExecutor {
    fn init(&mut self, ctx: &mut ExecutorContext<'_>) -> Result<(), ExecutorError> {
        let table = ctx.catalog.get_table_by_id(self.table_id)?;
        ctx.lock_manager.lock_table(ctx.txn.txn_id, self.table_id, LockMode::Shared)?;
        self.column_types = table.schema.columns().iter().map(|column| column.data_type).collect();
        self.table_first_page_id = Some(table.first_page_id);

        let root_page_id = ctx.catalog.index_root_page(self.index_id)?;
        let index = BTreeIndex::open(ctx.buffer_pool, root_page_id);
        let leaf = index.leaf_for_start(self.start.as_deref())?;
        self.current_leaf = Some(leaf);
        self.next_after = self.start.clone();
        Ok(())
    }

    fn next(&mut self, ctx: &mut ExecutorContext<'_>) -> Result<Option<Tuple>, ExecutorError> {
        let table_first_page_id = self.table_first_page_id.ok_or_else(|| {
            ExecutorError::Evaluation("IndexScanExecutor::next called before init".to_string())
        })?;
        let heap = TableHeap::open(ctx.buffer_pool, table_first_page_id);

        while let Some(leaf) = self.current_leaf {
            match BTreeIndex::scan_leaf(ctx.buffer_pool, leaf, self.next_after.as_deref())? {
                LeafScan::Entry { key, sort_key, rid, .. } => {
                    self.next_after = Some(sort_key);
                    if self.end.as_ref().is_some_and(|end| key.as_slice() >= end.as_slice()) {
                        self.current_leaf = None;
                        return Ok(None);
                    }
                    ctx.lock_manager.lock(ctx.txn.txn_id, rid, LockMode::Shared)?;
                    let bytes = heap.get_tuple(rid)?.ok_or_else(|| {
                        ExecutorError::CorruptTuple(format!(
                            "index entry points at a missing heap row: {rid:?}"
                        ))
                    })?;
                    let tuple = Tuple::decode(&bytes, &self.column_types)
                        .map_err(|err| ExecutorError::CorruptTuple(err.to_string()))?;
                    return Ok(Some(tuple));
                }
                LeafScan::EndOfLeaf { next_leaf_page_id } => {
                    self.current_leaf = next_leaf_page_id;
                }
            }
        }
        Ok(None)
    }
}
