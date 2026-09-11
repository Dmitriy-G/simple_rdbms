use types::Tuple;

use crate::context::ExecutorContext;
use crate::error::ExecutorError;
use crate::executor::Executor;

pub struct OneRowExecutor {
    emitted: bool,
}

impl OneRowExecutor {
    pub fn new() -> Self {
        Self { emitted: false }
    }
}

impl Default for OneRowExecutor {
    fn default() -> Self {
        Self::new()
    }
}

impl Executor for OneRowExecutor {
    fn init(&mut self, _ctx: &mut ExecutorContext<'_>) -> Result<(), ExecutorError> {
        self.emitted = false;
        Ok(())
    }

    fn next(&mut self, _ctx: &mut ExecutorContext<'_>) -> Result<Option<Tuple>, ExecutorError> {
        if self.emitted {
            return Ok(None);
        }
        self.emitted = true;
        Ok(Some(Tuple::new(Vec::new())))
    }
}
