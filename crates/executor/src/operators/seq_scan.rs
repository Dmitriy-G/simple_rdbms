use common::TableId;
use types::Tuple;

use crate::context::ExecutorContext;
use crate::error::ExecutorError;
use crate::executor::Executor;

/// Reads every tuple of a table's heap file, in physical storage order.
/// The leaf operator for any plan that reads a table without an index.
pub struct SeqScanExecutor {
    #[allow(dead_code)]
    table_id: TableId,
}

impl SeqScanExecutor {
    /// Creates a scan over `table_id`.
    pub fn new(table_id: TableId) -> Self {
        Self { table_id }
    }
}

impl Executor for SeqScanExecutor {
    fn init(&mut self, ctx: &mut ExecutorContext<'_>) -> Result<(), ExecutorError> {
        let _ = ctx;
        todo!("look up the table's TableInfo in the catalog, open a HeapFile iterator")
    }

    fn next(&mut self, ctx: &mut ExecutorContext<'_>) -> Result<Option<Tuple>, ExecutorError> {
        let _ = ctx;
        todo!("pull the next (Rid, bytes) from the heap iterator and decode it into a Tuple")
    }
}
