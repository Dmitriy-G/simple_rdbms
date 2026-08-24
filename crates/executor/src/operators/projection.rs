use planner::BoundExpr;
use types::Tuple;

use crate::context::ExecutorContext;
use crate::error::ExecutorError;
use crate::executor::Executor;
use crate::expression::evaluate;

pub struct ProjectionExecutor {
    expressions: Vec<BoundExpr>,
    child: Box<dyn Executor>,
}

impl ProjectionExecutor {
    pub fn new(expressions: Vec<BoundExpr>, child: Box<dyn Executor>) -> Self {
        Self { expressions, child }
    }
}

impl Executor for ProjectionExecutor {
    fn init(&mut self, ctx: &mut ExecutorContext<'_>) -> Result<(), ExecutorError> {
        self.child.init(ctx)
    }

    fn next(&mut self, ctx: &mut ExecutorContext<'_>) -> Result<Option<Tuple>, ExecutorError> {
        let Some(tuple) = self.child.next(ctx)? else {
            return Ok(None);
        };
        let values = self
            .expressions
            .iter()
            .map(|expr| evaluate(expr, &tuple))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Some(Tuple::new(values)))
    }
}
