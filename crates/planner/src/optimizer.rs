use crate::logical_plan::LogicalPlan;

/// A single rewrite rule applied during logical optimization, e.g. pushing
/// a `Filter` below a `Projection`, or eliminating a filter that is
/// statically always true. Each rule is self-contained so the optimizer can
/// run a fixed or configurable set of them.
pub trait OptimizerRule {
    /// A short, human-readable name for logging and `EXPLAIN` output.
    fn name(&self) -> &'static str;

    /// Applies this rule to `plan`, returning the rewritten plan. Returns
    /// `plan` unchanged if the rule does not apply.
    fn apply(&self, plan: LogicalPlan) -> LogicalPlan;
}

/// Runs a configured sequence of `OptimizerRule`s over a `LogicalPlan`
/// until it stops changing (or a fixed number of passes is reached), then
/// hands the result off for physical planning.
pub struct Optimizer {
    rules: Vec<Box<dyn OptimizerRule>>,
}

impl Optimizer {
    /// Creates an optimizer that applies `rules` in order.
    pub fn new(rules: Vec<Box<dyn OptimizerRule>>) -> Self {
        Self { rules }
    }

    /// Applies every configured rule to `plan` and returns the rewritten
    /// tree.
    pub fn optimize(&self, plan: LogicalPlan) -> LogicalPlan {
        let _ = &self.rules;
        let _ = &plan;
        todo!("fold plan through each rule, recursing into child plan nodes")
    }
}
