#![forbid(unsafe_code)]

mod context;
mod error;
mod executor;
mod expression;
mod operators;

pub use context::ExecutorContext;
pub use error::ExecutorError;
pub use executor::Executor;
pub use expression::evaluate;
pub use operators::{
    FilterExecutor, IndexScanExecutor, InsertExecutor, NestedLoopJoinExecutor, ProjectionExecutor,
    SeqScanExecutor,
};
