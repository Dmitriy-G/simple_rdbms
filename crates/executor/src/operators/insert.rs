use common::{PageId, TableId};
use planner::BoundExpr;
use storage::heap::TableHeap;
use types::{Encode, Tuple, Value};

use crate::context::ExecutorContext;
use crate::error::ExecutorError;
use crate::executor::Executor;
use crate::expression::evaluate;

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
        let table = ctx
            .catalog
            .get_table_by_id(self.table_id)
            .map_err(|err| ExecutorError::Catalog(err.to_string()))?;
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

        let empty = Tuple::new(Vec::new());
        let mut count: i64 = 0;
        for row in &self.rows {
            let values =
                row.iter().map(|expr| evaluate(expr, &empty)).collect::<Result<Vec<_>, _>>()?;
            let mut bytes = Vec::new();
            Tuple::new(values).encode(&mut bytes);
            heap.insert_tuple(ctx.txn.txn_id, &bytes)?;
            count += 1;
        }

        Ok(Some(Tuple::new(vec![Value::BigInt(count)])))
    }
}
