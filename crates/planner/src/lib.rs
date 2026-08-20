//! Query planning: binds a parsed `sql::Statement` against the `catalog`
//! into a type-checked, name-resolved tree, lowers that into a
//! `LogicalPlan`, and (eventually) rewrites and chooses physical operators
//! to produce a `PhysicalPlan` the `executor` can run.
//!
//! The three representations stay separate on purpose: `BoundStatement`
//! still mirrors the shape of the SQL statement it came from (one bound
//! node per clause), `LogicalPlan` is a relational-algebra tree independent
//! of SQL syntax, and `PhysicalPlan` additionally commits to *how* each
//! logical operation will be executed (e.g. which join algorithm).

#![forbid(unsafe_code)]

mod binder;
mod error;
mod logical_plan;
mod optimizer;
mod physical_plan;

pub use binder::{
    Binder, BoundColumnDef, BoundCreateTable, BoundExpr, BoundInsert, BoundSelect, BoundStatement,
};
pub use error::PlannerError;
pub use logical_plan::LogicalPlan;
pub use optimizer::{Optimizer, OptimizerRule};
pub use physical_plan::PhysicalPlan;
