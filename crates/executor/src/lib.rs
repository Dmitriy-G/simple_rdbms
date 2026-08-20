//! The query executor: turns a `planner::PhysicalPlan` into a tree of
//! pull-based (Volcano-model) operators and drives it to produce
//! `types::Tuple`s.
//!
//! Every operator implements `Executor`. A caller calls `init` once, then
//! calls `next` repeatedly until it returns `Ok(None)`; each operator pulls
//! from its children the same way, so the whole tree advances one tuple at
//! a time without materializing intermediate results.

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
    FilterExecutor, InsertExecutor, NestedLoopJoinExecutor, ProjectionExecutor, SeqScanExecutor,
};
