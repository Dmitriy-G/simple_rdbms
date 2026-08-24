# filter

Pulls tuples from `child`, discarding any for which `predicate` does not
evaluate to `true`. `NULL` is not `true`: a row whose predicate is
unknown is excluded, same as a row where it is definitely `false`.

## Key Components

- `FilterExecutor` - holds `predicate: BoundExpr` and `child: Box<dyn
  Executor>`.
- `FilterExecutor::new(predicate, child) -> Self` - creates a filter over
  `child`'s output using `predicate`.

## Usage Example

```rust
use executor::{Executor, FilterExecutor};

let filtered = FilterExecutor::new(predicate, Box::new(scan));
```

`engine::executor_factory::build_executor` constructs a `FilterExecutor`
for every `PhysicalPlan::Filter`, wrapping whatever operator its `input`
lowers to; a `WHERE` clause in a `SELECT` is what puts a `Filter` node in
the plan in the first place.
