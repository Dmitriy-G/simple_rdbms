# planner

Query planning: binds a parsed `sql::Statement` against the `catalog` into
a type-checked, name-resolved tree, lowers that into a `LogicalPlan`,
rewrites it (choosing an index scan over a sequential scan where one
applies), and lowers the result into a `PhysicalPlan` the `executor` can
run.

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
planner::plan -> LogicalPlan -> Optimizer::optimize -> LogicalPlan ->
to_physical -> PhysicalPlan` for a `SELECT`; `INSERT`/`CREATE TABLE`/
`CREATE INDEX` skip `Optimizer` (see Features).

## Key Components

- `binder` - `Binder`, `BoundColumnDef`, `BoundCreateIndex`,
  `BoundCreateTable`, `BoundExpr`, `BoundInsert`, `BoundSelect`,
  `BoundStatement`: binds parsed `sql` AST nodes against a `Catalog`. See
  [binder.MD](src/binder.MD).
- `error` - `PlannerError`, errors raised while binding. See
  [error.MD](src/error.MD).
- `logical_plan` - `LogicalPlan`, a relational-algebra plan tree. See
  [logical_plan.MD](src/logical_plan.MD).
- `optimizer` - `Optimizer`, `OptimizerRule`, `IndexScanRule`: rewrites a
  `LogicalPlan` against a `Catalog`. See [optimizer.MD](src/optimizer.MD).
- `physical_plan` - `PhysicalPlan`, `to_physical`: a plan tree committed to
  concrete execution algorithms. See
  [physical_plan.MD](src/physical_plan.MD).
- `plan` - lowers a `BoundStatement` into a `LogicalPlan`. See
  [plan.MD](src/plan.MD).
- `explain` - `explain_logical`, `explain_physical`: render either plan
  tree as `EXPLAIN`'s human-readable output against a `Catalog`. See
  [explain.MD](src/explain.MD).

`BinaryOperator`/`UnaryOperator` are also part of this crate's public API,
re-exported from `sql` rather than duplicated, so a downstream crate that
only sees `BoundExpr` (whose variants carry these operator types) can name
them without depending on `sql` directly — a dependency edge the
workspace's rules don't allow for `executor`.

## Features

Binding and lowering work end to end for `CREATE TABLE`, `CREATE INDEX`,
`INSERT`, and `SELECT` (projection and a `WHERE` filter, single table).
`LogicalPlan` and `PhysicalPlan` both have a `Join`/`NestedLoopJoin` node
kind and `to_physical` maps one to the other, but nothing produces a
`Join` node from real SQL today — `sql` has no `JOIN` syntax — so it's
only reachable by constructing a `LogicalPlan` directly, which is how
`executor`'s own join tests exercise it.

`Optimizer::optimize` runs a single bottom-up fold of the configured rules
over a `LogicalPlan` (no iterate-to-fixpoint loop). `IndexScanRule` is the
one rule so far: it rewrites `Filter { predicate, input: SeqScan }` into
`Filter { predicate, input: IndexScan }` when `predicate` has a conjunct
`ColumnRef <op> Literal` (`op` in `=,<,<=,>,>=`) against an indexed
column, deriving `[start, end)` bounds and combining multiple conjuncts on
the same column — always keeping the `Filter` on top so the rewrite is
correct by construction. Choosing *among* several viable access paths by
estimated cost, and dropping a filter an equality index scan alone could
already satisfy, are both left to roadmap milestone M11 — see
`docs/ROADMAP.md`.

`EXPLAIN [VERBOSE] <statement>` binds and lowers its target exactly as if
run directly, then hands the resulting `LogicalPlan`/`PhysicalPlan` to
`explain_logical`/`explain_physical` instead of to the executor —
`engine::Database::execute` never begins a transaction or touches the
buffer pool beyond the already-resident `Catalog` to answer one.

## Dependencies

Workspace: `common`, `types` (`types::MemcomparableEncode` for
`IndexScanRule`'s bound derivation), `catalog`, `sql`. External:
`thiserror`, for `PlannerError`.

## Configuration

None — `planner` has no configuration of its own.

## Testing

`tests/binder_tests.rs` binds parsed statements against a populated catalog
and checks that unknown columns/tables and type mismatches are rejected.
`tests/optimizer_tests.rs` runs `Optimizer` (with `IndexScanRule`) over
bound-and-planned `SELECT`s against a `Catalog::from_tables_and_indexes`
fixture, checking that an indexed predicate is rewritten to an `IndexScan`
(and a non-indexed one, or a table with no index at all, is not), that
each comparison operator derives the expected bound shape, and that a
compound `AND` on the same indexed column tightens both bounds.
`tests/smoke.rs` is the minimum-viable compile-and-construct check. Run
just this crate with:

```sh
cargo test -p planner
```
