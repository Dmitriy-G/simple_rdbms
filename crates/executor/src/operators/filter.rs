use planner::BoundExpr;
use types::Tuple;

use crate::context::ExecutorContext;
use crate::error::ExecutorError;
use crate::executor::Executor;

/// Pulls tuples from `child`, discarding any for which `predicate` does not
/// evaluate to `true`.
pub struct FilterExecutor {
    #[allow(dead_code)]
    predicate: BoundExpr,
    #[allow(dead_code)]
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
        let _ = ctx;
        todo!("init the child executor")
    }

    fn next(&mut self, ctx: &mut ExecutorContext<'_>) -> Result<Option<Tuple>, ExecutorError> {
        let _ = ctx;
        todo!("loop pulling from child, evaluating predicate, until a match or exhaustion")
    }
}
