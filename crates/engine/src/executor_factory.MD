# executor_factory

Builds an operator tree from a `PhysicalPlan`, one `Executor` per node.
The small factory `execute` needs to turn a plan into something it can
actually pull tuples from.

`PhysicalPlan::CreateTable` never reaches here: `Database::execute`
handles `CREATE TABLE` directly against the catalog before physical
planning, since it has no tuple stream to pull.

## Key Components

- `build_executor(plan) -> Box<dyn Executor>` - recursively lowers a
  `PhysicalPlan` into an `executor` operator tree: `SeqScan` to
  `SeqScanExecutor`, `Filter` to `FilterExecutor` wrapping its lowered
  input, `Projection` to `ProjectionExecutor` wrapping its lowered input,
  `NestedLoopJoin` to `NestedLoopJoinExecutor` wrapping both lowered
  sides, `Insert` to `InsertExecutor`. `CreateTable` is unreachable here.

## Usage Example

```rust
use crate::executor_factory::build_executor;

let mut executor = build_executor(physical_plan);
executor.init(&mut ctx)?;
while let Some(tuple) = executor.next(&mut ctx)? {
    rows.push(tuple);
}
```

`Database::run` is the sole caller: it calls `build_executor` once per
statement that reaches physical planning, then drives the resulting tree
to exhaustion to collect the statement's output rows.
