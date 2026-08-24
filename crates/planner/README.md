# planner

Query planning: binds a parsed `sql::Statement` against the `catalog` into
a type-checked, name-resolved tree, lowers that into a `LogicalPlan`, and
(eventually) rewrites and chooses physical operators to produce a
`PhysicalPlan` the `executor` can run.

## Architecture

`planner` depends on `common`, `types`, `catalog`, and `sql`, and sits
below `executor`/`engine` (see `docs/adr/0002-crate-splitting.md`) — it is
the one crate that knows about both SQL syntax and catalog metadata at
once, since binding is exactly the act of resolving one against the other.

Three representations stay deliberately separate rather than collapsing
into one tree that grows an "is this bound yet / is this physical yet"
flag: `BoundStatement` still mirrors the shape of the SQL statement it came
from (one bound node per clause) and is where name resolution and type
checking happen (`Binder::bind`); `LogicalPlan` is a relational-algebra
tree independent of SQL syntax; `PhysicalPlan` additionally commits to
*how* each logical operation will be executed (e.g. which join algorithm).
The pipeline is `sql::Statement -> Binder::bind -> BoundStatement ->
planner::plan -> LogicalPlan -> to_physical -> PhysicalPlan`, with
`Optimizer::optimize` meant to sit between the last two steps once it does
anything (see Features).

## Key Components

- `binder` - `Binder`, `BoundColumnDef`, `BoundCreateTable`, `BoundExpr`,
  `BoundInsert`, `BoundSelect`, `BoundStatement`: binds parsed `sql` AST
  nodes against a `Catalog`. See [binder.MD](src/binder.MD).
- `error` - `PlannerError`, errors raised while binding. See
  [error.MD](src/error.MD).
- `logical_plan` - `LogicalPlan`, a relational-algebra plan tree. See
  [logical_plan.MD](src/logical_plan.MD).
- `optimizer` - `Optimizer`, `OptimizerRule`: rewrites a `LogicalPlan`. See
  [optimizer.MD](src/optimizer.MD).
- `physical_plan` - `PhysicalPlan`, `to_physical`: a plan tree committed to
  concrete execution algorithms. See
  [physical_plan.MD](src/physical_plan.MD).
- `plan` - lowers a `BoundStatement` into a `LogicalPlan`. See
  [plan.MD](src/plan.MD).

`BinaryOperator`/`UnaryOperator` are also part of this crate's public API,
re-exported from `sql` rather than duplicated, so a downstream crate that
only sees `BoundExpr` (whose variants carry these operator types) can name
them without depending on `sql` directly — a dependency edge the
workspace's rules don't allow for `executor`.

## Features

Binding and lowering work end to end for `CREATE TABLE`, `INSERT`, and
`SELECT` (projection and a `WHERE` filter, single table). `LogicalPlan`
and `PhysicalPlan` both have a `Join`/`NestedLoopJoin` node kind and
`to_physical` maps one to the other, but nothing produces a `Join` node
from real SQL today — `sql` has no `JOIN` syntax — so it's only reachable
by constructing a `LogicalPlan` directly, which is how `executor`'s own
join tests exercise it.

`Optimizer::optimize` is `todo!("fold plan through each rule, recursing
into child plan nodes")` — the `Optimizer`/`OptimizerRule` scaffolding
exists, but nothing calls `optimize` anywhere in `engine` yet, and no rule
implementations exist. That's roadmap milestone M11 (cost-based
optimization across join algorithms and access paths) — see
`docs/ROADMAP.md`.

## Dependencies

Workspace: `common`, `types`, `catalog`, `sql`. External: `thiserror`, for
`PlannerError`.

## Configuration

None — `planner` has no configuration of its own.

## Testing

`tests/binder_tests.rs` binds parsed statements against a populated catalog
and checks that unknown columns/tables and type mismatches are rejected.
`tests/smoke.rs` is the minimum-viable compile-and-construct check. Run
just this crate with:

```sh
cargo test -p planner
```
