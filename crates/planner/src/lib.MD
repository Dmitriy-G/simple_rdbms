# lib

Query planning: binds a parsed `sql::Statement` against the `catalog` into
a type-checked, name-resolved tree, lowers that into a `LogicalPlan`,
rewrites it (choosing an index scan over a sequential scan where one
applies), and lowers the result into a `PhysicalPlan` the `executor` can
run.

The three representations stay separate on purpose: `BoundStatement`
still mirrors the shape of the SQL statement it came from (one bound node
per clause), `LogicalPlan` is a relational-algebra tree independent of SQL
syntax, and `PhysicalPlan` additionally commits to *how* each logical
operation will be executed (e.g. which join algorithm).

## Key Components

- `Binder`, `BoundColumnDef`, `BoundCreateIndex`, `BoundCreateTable`,
  `BoundExpr`, `BoundInsert`, `BoundSelect`, `BoundStatement` - binds
  parsed `sql` AST nodes against a `Catalog`. See `binder.MD`.
- `PlannerError` - errors raised while binding. See `error.MD`.
- `LogicalPlan` - a relational-algebra plan tree. See `logical_plan.MD`.
- `Optimizer`, `OptimizerRule`, `IndexScanRule` - rewrites a `LogicalPlan`
  against a `Catalog`; `IndexScanRule` is the first rule, choosing an
  index scan over a sequential scan where one applies. See `optimizer.MD`.
- `PhysicalPlan`, `to_physical` - a plan tree committed to concrete
  execution algorithms. See `physical_plan.MD`.
- `plan` - lowers a `BoundStatement` into a `LogicalPlan`. See `plan.MD`.
- `explain_logical`, `explain_physical` - render a `LogicalPlan`/
  `PhysicalPlan` as `EXPLAIN`'s human-readable node tree against a
  `Catalog`. See `explain.MD`.
- `BinaryOperator`, `UnaryOperator` - re-exported from `sql`, so
  downstream crates that only see `BoundExpr` (whose `BinaryOp`/`UnaryOp`
  variants carry these operator types) can name them without depending on
  `sql` directly, which the workspace's dependency-edge rules do not allow
  for e.g. `executor`.

## Usage Example

```rust
use planner::{Binder, IndexScanRule, Optimizer, to_physical};

let bound = Binder::new(&catalog).bind(statement)?;
let logical = planner::plan(bound)?;
let logical = Optimizer::new(vec![Box::new(IndexScanRule)]).optimize(logical, &catalog);
let physical = to_physical(logical);
```

`engine::Database::execute_bound`'s `Select` arm runs exactly this
pipeline: bind against the live catalog, lower to a logical plan,
optimize it, then lower to a physical plan, which
`engine::executor_factory::build_executor` turns into an operator tree
for `executor` to run. `INSERT` binds and lowers the same way but skips
the `Optimizer` step - there is nothing in an `Insert` plan for
`IndexScanRule` to act on. `CREATE TABLE`/`CREATE INDEX` bind the same
way but are handled directly by `Database::execute_bound` rather than
reaching `plan`/`Optimizer`/the executor at all, since they mutate the
catalog itself.
