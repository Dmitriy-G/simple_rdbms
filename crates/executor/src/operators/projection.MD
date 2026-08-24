# projection

Pulls tuples from `child` and evaluates `expressions` against each one,
producing a tuple of the results in place of the input row.

## Key Components

- `ProjectionExecutor` - holds `expressions: Vec<BoundExpr>` and `child:
  Box<dyn Executor>`.
- `ProjectionExecutor::new(expressions, child) -> Self` - creates a
  projection of `expressions` over `child`'s output.

## Usage Example

```rust
use executor::{Executor, ProjectionExecutor};

let projected = ProjectionExecutor::new(select_list, Box::new(filtered));
```

`engine::executor_factory::build_executor` constructs a
`ProjectionExecutor` for every `PhysicalPlan::Projection`; this is what
narrows and reorders a `SeqScan`'s (or `Filter`'s) output down to the
columns a `SELECT` list actually asked for.
