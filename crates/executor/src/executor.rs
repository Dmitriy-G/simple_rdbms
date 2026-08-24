use types::Tuple;

use crate::context::ExecutorContext;
use crate::error::ExecutorError;

pub trait Executor {
    fn init(&mut self, ctx: &mut ExecutorContext<'_>) -> Result<(), ExecutorError>;

    fn next(&mut self, ctx: &mut ExecutorContext<'_>) -> Result<Option<Tuple>, ExecutorError>;
}
