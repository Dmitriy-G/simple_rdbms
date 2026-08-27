use common::{IndexId, PageId, TableId};
use planner::BoundExpr;
use storage::btree::BTreeIndex;
use storage::heap::TableHeap;
use types::{Encode, MemcomparableEncode, Tuple, Value};

use crate::context::ExecutorContext;
use crate::error::ExecutorError;
use crate::executor::Executor;
use crate::expression::evaluate;

struct IndexTarget {
    index_id: IndexId,
    column_index: usize,
    root_page_id: PageId,
}

pub struct InsertExecutor {
    table_id: TableId,
    rows: Vec<Vec<BoundExpr>>,
    first_page_id: Option<PageId>,
    done: bool,
}

impl InsertExecutor {
    pub fn new(table_id: TableId, rows: Vec<Vec<BoundExpr>>) -> Self {
        Self { table_id, rows, first_page_id: None, done: false }
    }
}

impl Executor for InsertExecutor {
    fn init(&mut self, ctx: &mut ExecutorContext<'_>) -> Result<(), ExecutorError> {
        let table = ctx.catalog.get_table_by_id(self.table_id)?;
        self.first_page_id = Some(table.first_page_id);
        Ok(())
    }

    fn next(&mut self, ctx: &mut ExecutorContext<'_>) -> Result<Option<Tuple>, ExecutorError> {
        if self.done {
            return Ok(None);
        }
        self.done = true;

        let first_page_id = self.first_page_id.ok_or_else(|| {
            ExecutorError::Evaluation("InsertExecutor::next called before init".to_string())
        })?;
        let mut heap = TableHeap::open(ctx.buffer_pool, first_page_id);

        let mut targets: Vec<IndexTarget> = ctx
            .catalog
            .indexes_for_table(self.table_id)
            .map(|index| IndexTarget {
                index_id: index.index_id,
                column_index: index.column_index,
                root_page_id: index.root_page_id(),
            })
            .collect();

        let empty = Tuple::new(Vec::new());
        let mut count: i64 = 0;
        for row in &self.rows {
            let values =
                row.iter().map(|expr| evaluate(expr, &empty)).collect::<Result<Vec<_>, _>>()?;
            let tuple = Tuple::new(values);
            let mut bytes = Vec::new();
            tuple.encode(&mut bytes);
            let rid = heap.insert_tuple(ctx.txn.txn_id, &bytes)?;

            for target in &mut targets {
                let value = &tuple.values()[target.column_index];
                let mut key = Vec::new();
                value.encode_memcomparable(&mut key).map_err(storage::StorageError::from)?;

                let mut btree_index = BTreeIndex::open(ctx.buffer_pool, target.root_page_id);
                btree_index.insert(ctx.txn.txn_id, &key, rid)?;
                let root_after = btree_index.root_page_id();
                if root_after != target.root_page_id {
                    ctx.catalog.update_index_root_page(
                        ctx.buffer_pool,
                        ctx.txn.txn_id,
                        target.index_id,
                        root_after,
                    )?;
                    target.root_page_id = root_after;
                }
            }

            count += 1;
        }

        Ok(Some(Tuple::new(vec![Value::BigInt(count)])))
    }
}
