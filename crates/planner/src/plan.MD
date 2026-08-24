# plan

Lowers a bound statement into a `LogicalPlan`.

## Key Components

- `plan(statement) -> Result<LogicalPlan, PlannerError>` - lowers a bound
  statement into a `LogicalPlan`. A `SELECT` always builds the fixed shape
  `Projection(Filter(Scan))` (dropping the `Filter` node when there is no
  `WHERE` clause); there is no optimizer yet to choose a different shape
  or access path (`// TODO(M8): cost-based optimization`).

## Usage Example

```rust
use planner::plan;

let logical = plan(bound_statement)?;
```

`engine::Database::execute` calls `plan` right after binding, on the way
to `physical_plan::to_physical` and then the executor.
