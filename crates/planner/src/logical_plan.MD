# logical_plan

A relational-algebra plan tree, independent of the SQL syntax it was bound
from. Each variant is a logical operation ("scan this table", "filter
these rows") without committing to an execution algorithm; that choice is
made when lowering to a `PhysicalPlan`.

## Key Components

- `LogicalPlan` - the enum. `SeqScan { table_id }` reads every row of a
  table. `Filter { predicate, input }` keeps only rows of `input` matching
  `predicate`. `Projection { expressions, input }` computes `expressions`
  over each row of `input`, in output column order. `Join { left, right,
  predicate }` joins the rows of `left` and `right` by nested-loop
  evaluation of `predicate` (the only join strategy modeled at the
  logical level for now). `Insert { table_id, rows }` inserts `rows` into
  a table. `CreateTable { table_name, columns }` creates a new table.

## Usage Example

```rust
use planner::LogicalPlan;

let scan = LogicalPlan::SeqScan { table_id };
let filtered = LogicalPlan::Filter { predicate, input: Box::new(scan) };
```

`plan::plan` builds a `LogicalPlan` from a `BoundStatement`; `Optimizer`
would rewrite it in place before `physical_plan::to_physical` lowers it
into a `PhysicalPlan`, though the optimizer itself is not yet implemented.
