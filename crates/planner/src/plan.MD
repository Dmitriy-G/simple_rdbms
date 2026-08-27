# plan

Lowers a bound statement into a `LogicalPlan`.

## Key Components

- `plan(statement) -> Result<LogicalPlan, PlannerError>` - lowers a bound
  statement into a `LogicalPlan`. A `SELECT` always builds the fixed shape
  `Projection(Filter(SeqScan))` (dropping the `Filter` node when there is
  no `WHERE` clause) - always a `SeqScan` at this stage, never an
  `IndexScan`; choosing an access path is `optimizer::IndexScanRule`'s job,
  run afterward, not this function's (`// TODO(M11): cost-based
  optimization` marks the larger, still-missing piece: choosing *among*
  multiple viable access paths by estimated cost, once more than one
  exists). `CreateTable`/`CreateIndex` each lower to their own
  identically-shaped `LogicalPlan` variant, present only so this match
  stays exhaustive - neither is ever fed through `Optimizer`/`to_physical`;
  `engine::Database::execute_bound` handles both directly against the
  catalog (see `logical_plan.MD`). `BoundStatement::Explain` hits this
  match's one `unreachable!` arm - `engine::Database`'s `EXPLAIN` handling
  unwraps the `Explain` wrapper and calls `plan` on its `inner` statement
  directly, so `plan` itself is never handed the wrapper.

## Usage Example

```rust
use planner::plan;

let logical = plan(bound_statement)?;
```

`engine::Database::execute_bound`'s `Select` arm calls `plan` right after
binding, then `optimizer::Optimizer::optimize`, then
`physical_plan::to_physical`, on the way to the executor.
