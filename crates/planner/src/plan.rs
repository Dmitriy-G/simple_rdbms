use crate::binder::BoundStatement;
use crate::error::PlannerError;
use crate::logical_plan::LogicalPlan;

// TODO(M8): cost-based optimization

/// Lowers a bound statement into a `LogicalPlan`. A `SELECT` always builds
/// the fixed shape `Projection(Filter(Scan))` (dropping the `Filter` node
/// when there is no `WHERE` clause); there is no optimizer yet to choose a
/// different shape or access path.
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
    };
    Ok(plan)
}
