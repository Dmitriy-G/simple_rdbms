use planner::BoundExpr;
use types::Tuple;

use crate::context::ExecutorContext;
use crate::error::ExecutorError;
use crate::executor::Executor;

/// Pulls tuples from `child` and evaluates `expressions` against each one,
/// producing a tuple of the results in place of the input row.
pub struct ProjectionExecutor {
    #[allow(dead_code)]
    expressions: Vec<BoundExpr>,
    #[allow(dead_code)]
    child: Box<dyn Executor>,
}

impl ProjectionExecutor {
    /// Creates a projection of `expressions` over `child`'s output.
    pub fn new(expressions: Vec<BoundExpr>, child: Box<dyn Executor>) -> Self {
        Self { expressions, child }
    }
}

impl Executor for ProjectionExecutor {
    fn init(&mut self, ctx: &mut ExecutorContext<'_>) -> Result<(), ExecutorError> {
        let _ = ctx;
        todo!("init the child executor")
    }

    fn next(&mut self, ctx: &mut ExecutorContext<'_>) -> Result<Option<Tuple>, ExecutorError> {
        let _ = ctx;
        todo!("pull the next row from child, evaluate each expression against it")
    }
}
