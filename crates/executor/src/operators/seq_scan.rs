use common::{PageId, Rid, TableId};
use storage::heap::{PageScan, TableHeap};
use txn::LockMode;
use types::{DataType, Tuple};

use crate::context::ExecutorContext;
use crate::error::ExecutorError;
use crate::executor::Executor;

pub struct SeqScanExecutor {
    table_id: TableId,
    column_types: Vec<DataType>,
    current_page: Option<PageId>,
    next_slot: u16,
}

impl SeqScanExecutor {
    pub fn new(table_id: TableId) -> Self {
        Self { table_id, column_types: Vec::new(), current_page: None, next_slot: 0 }
    }
}

impl Executor for SeqScanExecutor {
    fn init(&mut self, ctx: &mut ExecutorContext<'_>) -> Result<(), ExecutorError> {
        let table = ctx.catalog.get_table_by_id(self.table_id)?;
        ctx.lock_manager.lock_table(ctx.txn.txn_id, self.table_id, LockMode::Shared)?;
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
                    ctx.lock_manager.lock(
                        ctx.txn.txn_id,
                        Rid::new(page_id, slot),
                        LockMode::Shared,
                    )?;
                    let tuple = Tuple::decode(&bytes, &self.column_types)
                        .map_err(|err| ExecutorError::CorruptTuple(err.to_string()))?;
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
