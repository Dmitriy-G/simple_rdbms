# optimizer

Runs a configured sequence of rewrite rules over a `LogicalPlan` before
physical planning.

## Key Components

- `OptimizerRule` - a single rewrite rule applied during logical
  optimization, e.g. pushing a `Filter` below a `Projection`, or
  eliminating a filter that is statically always true. Each rule is
  self-contained so the optimizer can run a fixed or configurable set of
  them.
- `OptimizerRule::name(&self) -> &'static str` - a short, human-readable
  name for logging and `EXPLAIN` output.
- `OptimizerRule::apply(&self, plan) -> LogicalPlan` - applies this rule
  to `plan`, returning the rewritten plan. Returns `plan` unchanged if the
  rule does not apply.
- `Optimizer` - runs a configured sequence of `OptimizerRule`s over a
  `LogicalPlan` until it stops changing (or a fixed number of passes is
  reached), then hands the result off for physical planning.
- `Optimizer::new(rules) -> Self` - creates an optimizer that applies
  `rules` in order.
- `Optimizer::optimize(&self, plan) -> LogicalPlan` - applies every
  configured rule to `plan` and returns the rewritten tree. Unimplemented
  (`todo!`).

## Usage Example

```rust
use planner::Optimizer;

let optimizer = Optimizer::new(Vec::new());
let optimized = optimizer.optimize(logical_plan);
```

Not yet wired into `engine::Database::execute`, which currently lowers a
`LogicalPlan` straight to a `PhysicalPlan` via `to_physical` with no
optimization pass in between.
