use executor::{
    Executor, FilterExecutor, InsertExecutor, NestedLoopJoinExecutor, ProjectionExecutor,
    SeqScanExecutor,
};
use planner::PhysicalPlan;

/// Builds an operator tree from a `PhysicalPlan`, one `Executor` per node.
/// The small factory `execute` needs to turn a plan into something it can
/// actually pull tuples from.
///
/// `PhysicalPlan::CreateTable` never reaches here: `Database::execute`
/// handles `CREATE TABLE` directly against the catalog before physical
/// planning, since it has no tuple stream to pull.
pub fn build_executor(plan: PhysicalPlan) -> Box<dyn Executor> {
    match plan {
        PhysicalPlan::SeqScan { table_id } => Box::new(SeqScanExecutor::new(table_id)),
        PhysicalPlan::Filter { predicate, input } => {
            Box::new(FilterExecutor::new(predicate, build_executor(*input)))
        }
        PhysicalPlan::Projection { expressions, input } => {
            Box::new(ProjectionExecutor::new(expressions, build_executor(*input)))
        }
        PhysicalPlan::NestedLoopJoin { left, right, predicate } => Box::new(
            NestedLoopJoinExecutor::new(build_executor(*left), build_executor(*right), predicate),
        ),
        PhysicalPlan::Insert { table_id, rows } => Box::new(InsertExecutor::new(table_id, rows)),
        PhysicalPlan::CreateTable { .. } => {
            unreachable!("Database::execute handles CreateTable directly, before physical planning")
        }
    }
}
