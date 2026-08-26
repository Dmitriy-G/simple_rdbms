use common::{IndexId, TableId};

use crate::binder::{BoundColumnDef, BoundExpr};

#[derive(Debug, Clone)]
pub enum LogicalPlan {
    SeqScan { table_id: TableId },
    IndexScan { index_id: IndexId, table_id: TableId, start: Option<Vec<u8>>, end: Option<Vec<u8>> },
    Filter { predicate: BoundExpr, input: Box<LogicalPlan> },
    Projection { expressions: Vec<BoundExpr>, input: Box<LogicalPlan> },
    Join { left: Box<LogicalPlan>, right: Box<LogicalPlan>, predicate: BoundExpr },
    Insert { table_id: TableId, rows: Vec<Vec<BoundExpr>> },
    CreateTable { table_name: String, columns: Vec<BoundColumnDef> },
    CreateIndex { index_name: String, table_id: TableId, column_index: usize },
}
