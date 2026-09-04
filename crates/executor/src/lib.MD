# lib

The query executor: turns a `planner::PhysicalPlan` into a tree of
pull-based (Volcano-model) operators and drives it to produce
`types::Tuple`s.

Every operator implements `Executor`. A caller calls `init` once, then
calls `next` repeatedly until it returns `Ok(None)`; each operator pulls
from its children the same way, so the whole tree advances one tuple at a
time without materializing intermediate results.

## Key Components

- `ExecutorContext` - the shared state (catalog, buffer pool, transaction,
  lock manager) every operator needs while executing. See `context.MD`.
- `ExecutorError` - errors raised while executing a physical plan. See
  `error.MD`.
- `Executor` - the pull-based trait every operator implements. See
  `executor.MD`.
- `evaluate` - evaluates a bound expression against a tuple. See
  `expression.MD`.
- `FilterExecutor`, `IndexScanExecutor`, `InsertExecutor`,
  `NestedLoopJoinExecutor`, `ProjectionExecutor`, `SeqScanExecutor` - the
  concrete operators, one per physical plan node kind. See
  `operators/mod.MD`.

## Usage Example

```rust
use executor::{Executor, ExecutorContext};

let mut executor = engine::executor_factory::build_executor(physical_plan);
let mut ctx = ExecutorContext::new(&catalog, &buffer_pool, txn, &lock_manager);
executor.init(&mut ctx)?;

let mut rows = Vec::new();
while let Some(tuple) = executor.next(&mut ctx)? {
    rows.push(tuple);
}
```

`engine::Database::run` is the sole driver of this loop: it turns a
`planner::PhysicalPlan` into an operator tree via
`engine::executor_factory::build_executor`, then calls `init` once
followed by `next` until exhaustion, collecting every tuple produced. No
other crate constructs or drives an `Executor` directly.
