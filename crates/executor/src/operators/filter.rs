use planner::BoundExpr;
use types::{Tuple, Value};

use crate::context::ExecutorContext;
use crate::error::ExecutorError;
use crate::executor::Executor;
use crate::expression::evaluate;

pub struct FilterExecutor {
    predicate: BoundExpr,
    child: Box<dyn Executor>,
}

impl FilterExecutor {
    pub fn new(predicate: BoundExpr, child: Box<dyn Executor>) -> Self {
        Self { predicate, child }
    }
}

impl Executor for FilterExecutor {
    fn init(&mut self, ctx: &mut ExecutorContext<'_>) -> Result<(), ExecutorError> {
        self.child.init(ctx)
    }

    fn next(&mut self, ctx: &mut ExecutorContext<'_>) -> Result<Option<Tuple>, ExecutorError> {
        while let Some(tuple) = self.child.next(ctx)? {
            if evaluate(&self.predicate, &tuple)? == Value::Boolean(true) {
                return Ok(Some(tuple));
            }
        }
        Ok(None)
    }
}
