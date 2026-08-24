use executor::{
    Executor, FilterExecutor, InsertExecutor, NestedLoopJoinExecutor, ProjectionExecutor,
    SeqScanExecutor,
};
use planner::PhysicalPlan;

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
