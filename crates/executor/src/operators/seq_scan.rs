use std::collections::VecDeque;

use common::TableId;
use storage::heap::TableHeap;
use types::{DataType, Tuple};

use crate::context::ExecutorContext;
use crate::error::ExecutorError;
use crate::executor::Executor;

/// Reads every tuple of a table's heap file, in physical storage order.
/// The leaf operator for any plan that reads a table without an index.
///
/// `init` walks the whole heap and decodes every live tuple into `rows`;
/// `next` then just drains that queue. A truly lazy, page-at-a-time scan
/// would hold the `TableIter` itself across calls, but `TableIter` borrows
/// the buffer pool for as long as it lives, and each `next` call only gets a
/// fresh, short-lived borrow of the pool through `ExecutorContext` — there
/// is no lifetime that outlives one call for it to borrow into. Eagerly
/// decoding at `init` sidesteps that without needing the executor tree to
/// carry a lifetime parameter of its own.
pub struct SeqScanExecutor {
    table_id: TableId,
    rows: VecDeque<Tuple>,
}

impl SeqScanExecutor {
    /// Creates a scan over `table_id`.
    pub fn new(table_id: TableId) -> Self {
        Self { table_id, rows: VecDeque::new() }
    }
}

impl Executor for SeqScanExecutor {
    fn init(&mut self, ctx: &mut ExecutorContext<'_>) -> Result<(), ExecutorError> {
        let table = ctx
            .catalog
            .get_table_by_id(self.table_id)
            .map_err(|err| ExecutorError::Catalog(err.to_string()))?;
        let column_types: Vec<DataType> =
            table.schema.columns().iter().map(|column| column.data_type).collect();

        let heap = TableHeap::open(ctx.buffer_pool, table.first_page_id);
        self.rows.clear();
        for entry in heap.iter() {
            let (_, bytes) = entry?;
            let tuple = Tuple::decode(&bytes, &column_types)
                .map_err(|err| ExecutorError::Evaluation(err.to_string()))?;
            self.rows.push_back(tuple);
        }
        Ok(())
    }

    fn next(&mut self, ctx: &mut ExecutorContext<'_>) -> Result<Option<Tuple>, ExecutorError> {
        let _ = ctx;
        Ok(self.rows.pop_front())
    }
}
