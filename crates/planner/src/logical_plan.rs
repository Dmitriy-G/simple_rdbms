use common::TableId;

use crate::binder::{BoundColumnDef, BoundExpr};

#[derive(Debug, Clone)]
pub enum LogicalPlan {
    SeqScan { table_id: TableId },
    Filter { predicate: BoundExpr, input: Box<LogicalPlan> },
    Projection { expressions: Vec<BoundExpr>, input: Box<LogicalPlan> },
    Join { left: Box<LogicalPlan>, right: Box<LogicalPlan>, predicate: BoundExpr },
    Insert { table_id: TableId, rows: Vec<Vec<BoundExpr>> },
    CreateTable { table_name: String, columns: Vec<BoundColumnDef> },
}
