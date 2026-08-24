# lib

Query planning: binds a parsed `sql::Statement` against the `catalog` into
a type-checked, name-resolved tree, lowers that into a `LogicalPlan`, and
(eventually) rewrites and chooses physical operators to produce a
`PhysicalPlan` the `executor` can run.

The three representations stay separate on purpose: `BoundStatement`
still mirrors the shape of the SQL statement it came from (one bound node
per clause), `LogicalPlan` is a relational-algebra tree independent of SQL
syntax, and `PhysicalPlan` additionally commits to *how* each logical
operation will be executed (e.g. which join algorithm).

## Key Components

- `Binder`, `BoundColumnDef`, `BoundCreateTable`, `BoundExpr`,
  `BoundInsert`, `BoundSelect`, `BoundStatement` - binds parsed `sql` AST
  nodes against a `Catalog`. See `binder.MD`.
- `PlannerError` - errors raised while binding. See `error.MD`.
- `LogicalPlan` - a relational-algebra plan tree. See `logical_plan.MD`.
- `Optimizer`, `OptimizerRule` - rewrites a `LogicalPlan`. See
  `optimizer.MD`.
- `PhysicalPlan`, `to_physical` - a plan tree committed to concrete
  execution algorithms. See `physical_plan.MD`.
- `plan` - lowers a `BoundStatement` into a `LogicalPlan`. See `plan.MD`.
- `BinaryOperator`, `UnaryOperator` - re-exported from `sql`, so
  downstream crates that only see `BoundExpr` (whose `BinaryOp`/`UnaryOp`
  variants carry these operator types) can name them without depending on
  `sql` directly, which the workspace's dependency-edge rules do not allow
  for e.g. `executor`.

## Usage Example

```rust
use planner::{Binder, to_physical};

let bound = Binder::new(&catalog).bind(statement)?;
let logical = planner::plan(bound)?;
let physical = to_physical(logical);
```

`engine::Database::execute` runs exactly this pipeline for every `SELECT`
and `INSERT`: bind against the live catalog, lower to a logical plan, then
to a physical plan, which `engine::executor_factory::build_executor` turns
into an operator tree for `executor` to run. `CREATE TABLE` binds and
lowers the same way but is handled directly by `Database::execute` rather
than reaching the executor, since it mutates the catalog itself.
