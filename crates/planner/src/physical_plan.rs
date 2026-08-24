use common::TableId;

use crate::binder::{BoundColumnDef, BoundExpr};
use crate::logical_plan::LogicalPlan;

#[derive(Debug, Clone)]
pub enum PhysicalPlan {
    SeqScan { table_id: TableId },
    Filter { predicate: BoundExpr, input: Box<PhysicalPlan> },
    Projection { expressions: Vec<BoundExpr>, input: Box<PhysicalPlan> },
    NestedLoopJoin { left: Box<PhysicalPlan>, right: Box<PhysicalPlan>, predicate: BoundExpr },
    Insert { table_id: TableId, rows: Vec<Vec<BoundExpr>> },
    CreateTable { table_name: String, columns: Vec<BoundColumnDef> },
}

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
