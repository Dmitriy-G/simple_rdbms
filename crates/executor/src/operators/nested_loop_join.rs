use planner::BoundExpr;
use types::Tuple;

use crate::context::ExecutorContext;
use crate::error::ExecutorError;
use crate::executor::Executor;

/// Joins `left` and `right` by, for each row of `left`, rescanning all of
/// `right` and keeping combined rows where `predicate` holds. The simplest
/// join algorithm and the only one this operator set supports; a
/// cost-based optimizer choosing between join algorithms is future work.
pub struct NestedLoopJoinExecutor {
    #[allow(dead_code)]
    left: Box<dyn Executor>,
    #[allow(dead_code)]
    right: Box<dyn Executor>,
    #[allow(dead_code)]
    predicate: BoundExpr,
}

impl NestedLoopJoinExecutor {
    /// Creates a nested-loop join of `left` (outer) and `right` (inner) on
    /// `predicate`.
    pub fn new(left: Box<dyn Executor>, right: Box<dyn Executor>, predicate: BoundExpr) -> Self {
        Self { left, right, predicate }
    }
}

impl Executor for NestedLoopJoinExecutor {
    fn init(&mut self, ctx: &mut ExecutorContext<'_>) -> Result<(), ExecutorError> {
        let _ = ctx;
        todo!("init both children, pull the first outer row")
    }

    fn next(&mut self, ctx: &mut ExecutorContext<'_>) -> Result<Option<Tuple>, ExecutorError> {
        let _ = ctx;
        todo!("advance the inner scan against the current outer row, re-driving on exhaustion")
    }
}
