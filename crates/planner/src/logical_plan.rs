use common::TableId;

use crate::binder::{BoundColumnDef, BoundExpr};

/// A relational-algebra plan tree, independent of the SQL syntax it was
/// bound from. Each variant is a logical operation ("scan this table",
/// "filter these rows") without committing to an execution algorithm; that
/// choice is made when lowering to a `PhysicalPlan`.
#[derive(Debug, Clone)]
pub enum LogicalPlan {
    /// Reads every row of a table.
    SeqScan {
        /// The table being scanned.
        table_id: TableId,
    },
    /// Keeps only rows of `input` matching `predicate`.
    Filter {
        /// The filter condition.
        predicate: BoundExpr,
        /// The plan producing the rows to filter.
        input: Box<LogicalPlan>,
    },
    /// Computes `expressions` over each row of `input`.
    Projection {
        /// The expressions to evaluate, in output column order.
        expressions: Vec<BoundExpr>,
        /// The plan producing the input rows.
        input: Box<LogicalPlan>,
    },
    /// Joins the rows of `left` and `right` by nested-loop evaluation of
    /// `predicate` (the only join strategy modeled at the logical level for
    /// now).
    Join {
        /// The outer input.
        left: Box<LogicalPlan>,
        /// The inner input.
        right: Box<LogicalPlan>,
        /// The join condition.
        predicate: BoundExpr,
    },
    /// Inserts the given rows into a table.
    Insert {
        /// The destination table.
        table_id: TableId,
        /// The rows to insert.
        rows: Vec<Vec<BoundExpr>>,
    },
    /// Creates a new table.
    CreateTable {
        /// The new table's name.
        table_name: String,
        /// The new table's columns.
        columns: Vec<BoundColumnDef>,
    },
}
