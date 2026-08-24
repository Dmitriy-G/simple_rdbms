# nested_loop_join

Joins `left` and `right` by, for each row of `left`, rescanning all of
`right` and keeping combined rows where `predicate` holds. The simplest
join algorithm and the only one this operator set supports; a cost-based
optimizer choosing between join algorithms is future work.

`init` and `next` are unimplemented (`todo!`): the struct, constructor,
and `Executor` impl exist so `engine::executor_factory::build_executor`
and the rest of the plan pipeline compile end-to-end against
`PhysicalPlan::NestedLoopJoin` before the pull loop itself is written.

## Key Components

- `NestedLoopJoinExecutor` - holds `left: Box<dyn Executor>` (outer),
  `right: Box<dyn Executor>` (inner), and `predicate: BoundExpr`; all
  three are currently `#[allow(dead_code)]` since neither `init` nor
  `next` reads them yet.
- `NestedLoopJoinExecutor::new(left, right, predicate) -> Self` - creates
  a nested-loop join of `left` (outer) and `right` (inner) on `predicate`.

## Usage Example

```rust
use executor::NestedLoopJoinExecutor;

let join = NestedLoopJoinExecutor::new(
    Box::new(left_scan),
    Box::new(right_scan),
    predicate,
);
// join.init(&mut ctx) and join.next(&mut ctx) currently panic (todo!)
```

`engine::executor_factory::build_executor` constructs a
`NestedLoopJoinExecutor` for every `PhysicalPlan::NestedLoopJoin`, but no
query that reaches one can currently run to completion: `init` must
initialize both children and pull the first outer row, and `next` must
advance the inner scan against the current outer row, re-driving the
inner scan and pulling a new outer row on its exhaustion.
