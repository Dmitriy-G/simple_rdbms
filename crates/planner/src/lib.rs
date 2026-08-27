#![forbid(unsafe_code)]

mod binder;
mod error;
pub mod explain;
mod logical_plan;
mod optimizer;
mod physical_plan;
mod plan;

pub use binder::{
    Binder, BoundColumnDef, BoundCreateIndex, BoundCreateTable, BoundExpr, BoundInsert,
    BoundSelect, BoundStatement,
};
pub use error::PlannerError;
pub use explain::{explain_logical, explain_physical};
pub use logical_plan::LogicalPlan;
pub use optimizer::{IndexScanRule, Optimizer, OptimizerRule};
pub use physical_plan::{PhysicalPlan, to_physical};
pub use plan::plan;
pub use sql::{BinaryOperator, UnaryOperator};
