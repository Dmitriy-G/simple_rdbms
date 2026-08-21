use common::TableId;

use crate::binder::{BoundColumnDef, BoundExpr};
use crate::logical_plan::LogicalPlan;

/// A plan tree that has committed to concrete execution algorithms for
/// every logical operation, ready for the `executor` to turn 1:1 into an
/// operator tree. Structurally mirrors `LogicalPlan`; the distinction
/// exists so the optimizer's algorithm-selection rules have a type that
/// can only represent "already decided" plans.
#[derive(Debug, Clone)]
pub enum PhysicalPlan {
    /// A sequential scan of a table's heap file.
    SeqScan {
        /// The table being scanned.
        table_id: TableId,
    },
    /// A row-at-a-time filter over its input.
    Filter {
        /// The filter condition.
        predicate: BoundExpr,
        /// The plan producing the rows to filter.
        input: Box<PhysicalPlan>,
    },
    /// A row-at-a-time projection over its input.
    Projection {
        /// The expressions to evaluate, in output column order.
        expressions: Vec<BoundExpr>,
        /// The plan producing the input rows.
        input: Box<PhysicalPlan>,
    },
    /// A nested-loop join: for each outer row, rescans the inner input
    /// looking for matches.
    NestedLoopJoin {
        /// The outer input, scanned once.
        left: Box<PhysicalPlan>,
        /// The inner input, rescanned per outer row.
        right: Box<PhysicalPlan>,
        /// The join condition.
        predicate: BoundExpr,
    },
    /// Inserts rows into a table's heap file.
    Insert {
        /// The destination table.
        table_id: TableId,
        /// The rows to insert.
        rows: Vec<Vec<BoundExpr>>,
    },
    /// Creates a new table in the catalog.
    CreateTable {
        /// The new table's name.
        table_name: String,
        /// The new table's columns.
        columns: Vec<BoundColumnDef>,
    },
}

/// Lowers a `LogicalPlan` into a `PhysicalPlan` by committing to the one
/// execution algorithm each logical operator currently has (a sequential
/// scan, a nested-loop join): a direct, unoptimized 1:1 mapping. Choosing
/// between multiple algorithms for the same logical operator is future work
/// for the cost-based optimizer (see `crate::optimizer`).
pub fn to_physical(plan: LogicalPlan) -> PhysicalPlan {
    match plan {
        LogicalPlan::SeqScan { table_id } => PhysicalPlan::SeqScan { table_id },
        LogicalPlan::Filter { predicate, input } => {
            PhysicalPlan::Filter { predicate, input: Box::new(to_physical(*input)) }
        }
        LogicalPlan::Projection { expressions, input } => {
            PhysicalPlan::Projection { expressions, input: Box::new(to_physical(*input)) }
        }
        LogicalPlan::Join { left, right, predicate } => PhysicalPlan::NestedLoopJoin {
            left: Box::new(to_physical(*left)),
            right: Box::new(to_physical(*right)),
            predicate,
        },
        LogicalPlan::Insert { table_id, rows } => PhysicalPlan::Insert { table_id, rows },
        LogicalPlan::CreateTable { table_name, columns } => {
            PhysicalPlan::CreateTable { table_name, columns }
        }
    }
}
