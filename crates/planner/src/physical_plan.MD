# physical_plan

A plan tree that has committed to concrete execution algorithms for every
logical operation, ready for the `executor` to turn 1:1 into an operator
tree. Structurally mirrors `LogicalPlan`; the distinction exists so the
optimizer's algorithm-selection rules have a type that can only represent
"already decided" plans.

## Key Components

- `PhysicalPlan` - the enum. `SeqScan { table_id }` is a sequential scan
  of a table's heap file. `IndexScan { index_id, table_id, start, end }`
  is a `storage::btree::BTreeIndex` range scan over `[start, end)`,
  fetching each matching row from the table's heap by `Rid` - mirrors
  `LogicalPlan::IndexScan` exactly, since there is only one execution
  algorithm for an index scan today. `Filter { predicate, input }` is a
  row-at-a-time filter over its input. `Projection { expressions, input }`
  is a row-at-a-time projection over its input, in output column order.
  `NestedLoopJoin { left, right, predicate }` is a nested-loop join: for
  each outer row, rescans the inner input looking for matches (`left` is
  the outer input, scanned once; `right` is the inner input, rescanned per
  outer row). `Insert { table_id, rows }` inserts rows into a table's heap
  file, maintaining every index on that table as it does
  (`executor::operators::insert.MD`). `CreateTable { table_name, columns }`
  creates a new table in the catalog. `CreateIndex { index_name, table_id,
  column_index }` creates a new index in the catalog - like
  `LogicalPlan::CreateIndex`, this variant is never actually constructed;
  see `logical_plan.MD`.
- `to_physical(plan) -> PhysicalPlan` - lowers a `LogicalPlan` into a
  `PhysicalPlan` by committing to the one execution algorithm each logical
  operator currently has (a sequential scan, an index scan, a nested-loop
  join): a direct, unoptimized 1:1 mapping. Choosing between multiple
  algorithms for the same logical operator is future work for the
  cost-based optimizer (see `optimizer.MD`).

## Usage Example

```rust
use planner::to_physical;

let physical = to_physical(logical_plan);
```

`engine::Database::execute_bound` calls `to_physical` right after
`Optimizer::optimize`; `engine::executor_factory::build_executor` then
matches on `PhysicalPlan` to build the corresponding `Executor` tree,
except `CreateTable`/`CreateIndex`, which `Database::execute_bound`
handles directly against the catalog rather than passing to the executor.
