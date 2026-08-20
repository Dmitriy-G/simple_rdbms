use types::Tuple;

use crate::context::ExecutorContext;
use crate::error::ExecutorError;

/// A single node in a pull-based (Volcano-model) operator tree. Every
/// physical plan node lowers to one `Executor`; a parent operator calls
/// `next` on its children to pull their output one tuple at a time, rather
/// than any operator materializing its whole result up front.
pub trait Executor {
    /// Prepares the executor to produce tuples: opens iterators, evaluates
    /// setup work, and recursively initializes any child executors. Must
    /// be called exactly once before the first call to `next`.
    fn init(&mut self, ctx: &mut ExecutorContext<'_>) -> Result<(), ExecutorError>;

    /// Pulls the next output tuple, or `Ok(None)` once the operator is
    /// exhausted. Must not be called again after returning `Ok(None)`.
    fn next(&mut self, ctx: &mut ExecutorContext<'_>) -> Result<Option<Tuple>, ExecutorError>;
}
