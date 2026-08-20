//! Concrete `Executor` implementations, one per physical plan node kind.

mod filter;
mod insert;
mod nested_loop_join;
mod projection;
mod seq_scan;

pub use filter::FilterExecutor;
pub use insert::InsertExecutor;
pub use nested_loop_join::NestedLoopJoinExecutor;
pub use projection::ProjectionExecutor;
pub use seq_scan::SeqScanExecutor;
