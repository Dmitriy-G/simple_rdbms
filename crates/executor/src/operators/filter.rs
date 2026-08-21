use planner::BoundExpr;
use types::{Tuple, Value};

use crate::context::ExecutorContext;
use crate::error::ExecutorError;
use crate::executor::Executor;
use crate::expression::evaluate;

/// Pulls tuples from `child`, discarding any for which `predicate` does not
/// evaluate to `true`.
pub struct FilterExecutor {
    predicate: BoundExpr,
    child: Box<dyn Executor>,
}

impl FilterExecutor {
    /// Creates a filter over `child`'s output using `predicate`.
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
            // `NULL` is not `true`: a row whose predicate is unknown is
            // excluded, same as a row where it is definitely `false`.
            if evaluate(&self.predicate, &tuple)? == Value::Boolean(true) {
                return Ok(Some(tuple));
            }
        }
        Ok(None)
    }
}
