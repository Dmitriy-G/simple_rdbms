use crate::logical_plan::LogicalPlan;

pub trait OptimizerRule {
    fn name(&self) -> &'static str;

    fn apply(&self, plan: LogicalPlan) -> LogicalPlan;
}

pub struct Optimizer {
    rules: Vec<Box<dyn OptimizerRule>>,
}

impl Optimizer {
    pub fn new(rules: Vec<Box<dyn OptimizerRule>>) -> Self {
        Self { rules }
    }

    pub fn optimize(&self, plan: LogicalPlan) -> LogicalPlan {
        let _ = &self.rules;
        let _ = &plan;
        todo!("fold plan through each rule, recursing into child plan nodes")
    }
}
