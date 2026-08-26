# mod

Concrete `Executor` implementations, one per physical plan node kind.

## Key Components

- `FilterExecutor` - re-exported from `filter`. See `filter.MD`.
- `IndexScanExecutor` - re-exported from `index_scan`. See `index_scan.MD`.
- `InsertExecutor` - re-exported from `insert`. See `insert.MD`.
- `NestedLoopJoinExecutor` - re-exported from `nested_loop_join`. See
  `nested_loop_join.MD`.
- `ProjectionExecutor` - re-exported from `projection`. See
  `projection.MD`.
- `SeqScanExecutor` - re-exported from `seq_scan`. See `seq_scan.MD`.

## Usage Example

```rust
use executor::{FilterExecutor, SeqScanExecutor};

let scan = SeqScanExecutor::new(table_id);
let filtered = FilterExecutor::new(predicate, Box::new(scan));
```

`engine::executor_factory::build_executor` matches on each
`planner::PhysicalPlan` variant and constructs exactly one of these
operators per node, wiring child operators together the same way a plan
tree nests its inputs.
