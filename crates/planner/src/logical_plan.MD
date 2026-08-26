# logical_plan

A relational-algebra plan tree, independent of the SQL syntax it was bound
from. Each variant is a logical operation ("scan this table", "filter
these rows") without committing to an execution algorithm; that choice is
made when lowering to a `PhysicalPlan`.

## Key Components

- `LogicalPlan` - the enum. `SeqScan { table_id }` reads every row of a
  table. `IndexScan { index_id, table_id, start, end }` reads the rows of
  `table_id` whose indexed column falls in `[start, end)` (either bound
  `None` for unbounded), via the index `index_id` - never constructed by
  `plan::plan` directly, only introduced by
  `optimizer::IndexScanRule` rewriting a `Filter` over a `SeqScan`; `start`/
  `end` are already `types::MemcomparableEncode`-encoded bytes, ready to
  pass straight to `storage::btree::BTreeIndex::range_scan`/`leaf_for_start`.
  `Filter { predicate, input }` keeps only rows of `input` matching
  `predicate`. `Projection { expressions, input }` computes `expressions`
  over each row of `input`, in output column order. `Join { left, right,
  predicate }` joins the rows of `left` and `right` by nested-loop
  evaluation of `predicate` (the only join strategy modeled at the
  logical level for now). `Insert { table_id, rows }` inserts `rows` into
  a table. `CreateTable { table_name, columns }` creates a new table.
  `CreateIndex { index_name, table_id, column_index }` creates a new
  index - like `CreateTable`, this variant exists only so `plan::plan`'s
  match over `BoundStatement` stays exhaustive; `engine::Database::execute_bound`
  handles `BoundStatement::CreateIndex` directly against the catalog and
  never actually constructs this variant (see `optimizer.MD`).

## Usage Example

```rust
use planner::LogicalPlan;

let scan = LogicalPlan::SeqScan { table_id };
let filtered = LogicalPlan::Filter { predicate, input: Box::new(scan) };
```

`plan::plan` builds a `LogicalPlan` from a `BoundStatement`; `Optimizer`
rewrites it in place (see `optimizer.MD` for `IndexScanRule`, the first
rule) before `physical_plan::to_physical` lowers it into a `PhysicalPlan`.
