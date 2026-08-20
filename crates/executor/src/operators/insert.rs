use common::TableId;
use types::Tuple;

use crate::context::ExecutorContext;
use crate::error::ExecutorError;
use crate::executor::Executor;

/// Pulls tuples from `child` and inserts each one into a table's heap
/// file. Yields no output rows of its own; a `SELECT`-less DML statement's
/// executor tree ends here.
pub struct InsertExecutor {
    #[allow(dead_code)]
    table_id: TableId,
    #[allow(dead_code)]
    child: Box<dyn Executor>,
}

impl InsertExecutor {
    /// Creates an insert into `table_id`, sourcing rows from `child`.
    pub fn new(table_id: TableId, child: Box<dyn Executor>) -> Self {
        Self { table_id, child }
    }
}

impl Executor for InsertExecutor {
    fn init(&mut self, ctx: &mut ExecutorContext<'_>) -> Result<(), ExecutorError> {
        let _ = ctx;
        todo!("look up the destination table's HeapFile handle, init the child executor")
    }

    fn next(&mut self, ctx: &mut ExecutorContext<'_>) -> Result<Option<Tuple>, ExecutorError> {
        let _ = ctx;
        todo!("pull every row from child, insert each into the heap, then return None")
    }
}
