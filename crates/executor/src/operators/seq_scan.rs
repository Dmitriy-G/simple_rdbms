use common::{PageId, TableId};
use storage::heap::{PageScan, TableHeap};
use types::{DataType, Tuple};

use crate::context::ExecutorContext;
use crate::error::ExecutorError;
use crate::executor::Executor;

/// Reads every tuple of a table's heap file, in physical storage order.
/// The leaf operator for any plan that reads a table without an index.
///
/// Holds only a cursor (`current_page`, `next_slot`) rather than a
/// materialized copy of the table: each `next` fetches the current page,
/// reads the next live slot on it, decodes one tuple, and drops the page's
/// guard before returning, following `next_page_id` once a page is
/// exhausted. Memory use is therefore independent of table size, and no
/// page stays pinned across a `next` call.
pub struct SeqScanExecutor {
    table_id: TableId,
    column_types: Vec<DataType>,
    current_page: Option<PageId>,
    next_slot: u16,
}

impl SeqScanExecutor {
    /// Creates a scan over `table_id`.
    pub fn new(table_id: TableId) -> Self {
        Self { table_id, column_types: Vec::new(), current_page: None, next_slot: 0 }
    }
}

impl Executor for SeqScanExecutor {
    fn init(&mut self, ctx: &mut ExecutorContext<'_>) -> Result<(), ExecutorError> {
        let table = ctx
            .catalog
            .get_table_by_id(self.table_id)
            .map_err(|err| ExecutorError::Catalog(err.to_string()))?;
        self.column_types = table.schema.columns().iter().map(|column| column.data_type).collect();
        self.current_page = Some(table.first_page_id);
        self.next_slot = 0;
        Ok(())
    }

    fn next(&mut self, ctx: &mut ExecutorContext<'_>) -> Result<Option<Tuple>, ExecutorError> {
        while let Some(page_id) = self.current_page {
            match TableHeap::scan_page(ctx.buffer_pool, page_id, self.next_slot)? {
                PageScan::Tuple { slot, bytes } => {
                    self.next_slot = slot + 1;
                    let tuple = Tuple::decode(&bytes, &self.column_types)
                        .map_err(|err| ExecutorError::Evaluation(err.to_string()))?;
                    return Ok(Some(tuple));
                }
                PageScan::EndOfPage { next_page_id } => {
                    self.current_page = next_page_id;
                    self.next_slot = 0;
                }
            }
        }
        Ok(None)
    }
}
