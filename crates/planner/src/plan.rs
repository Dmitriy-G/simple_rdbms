use crate::binder::BoundStatement;
use crate::error::PlannerError;
use crate::logical_plan::LogicalPlan;

// TODO(M11): cost-based optimization

pub fn plan(statement: BoundStatement) -> Result<LogicalPlan, PlannerError> {
    let plan = match statement {
        BoundStatement::Select(select) => {
            let scan = LogicalPlan::SeqScan { table_id: select.table_id };
            let input = match select.predicate {
                Some(predicate) => LogicalPlan::Filter { predicate, input: Box::new(scan) },
                None => scan,
            };
            LogicalPlan::Projection { expressions: select.projections, input: Box::new(input) }
        }
        BoundStatement::Insert(insert) => {
            LogicalPlan::Insert { table_id: insert.table_id, rows: insert.rows }
        }
        BoundStatement::CreateTable(create) => {
            LogicalPlan::CreateTable { table_name: create.table_name, columns: create.columns }
        }
        BoundStatement::CreateIndex(create) => LogicalPlan::CreateIndex {
            index_name: create.index_name,
            table_id: create.table_id,
            column_index: create.column_index,
        },
    };
    Ok(plan)
}
