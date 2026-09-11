# parameters

Parameter handling for prepared statements: works over a parsed but
unbound `sql::Statement`, before `binder::Binder` ever sees it, since the
binder rejects an `sql::Expr::Parameter` outright
(`binder.MD`'s `Binder::bind_expr`). This module is the one place a `$n`
placeholder is either given a type or replaced by a literal value; every
other layer either produces a placeholder (`sql::parser.MD`) or refuses
to see one (`binder.MD`).

## Key Components

- `infer_parameter_types(statement, catalog) -> Result<Vec<Option<DataType>>, PlannerError>` -
  walks `statement` without binding it, returning one entry per
  placeholder, indexed by `$n - 1`. An `INSERT INTO t (...) VALUES (...,
  $n, ...)` placeholder takes the type of the target column it lines up
  with positionally (following the statement's explicit column list when
  one is given, exactly like `binder::Binder::bind_insert`'s own
  `target_indices`). A placeholder on either side of a `BinaryOp` whose
  other side resolves to a column reference takes that column's type.
  Every other placeholder - one used bare in a select list, compared
  against another placeholder, or otherwise with no column in scope -
  resolves to `None` (unknown type; a caller such as
  `engine::Database::describe` is expected to fall back to `Varchar` for
  these). A `$n` with a lower-numbered placeholder missing (`$1` and `$3`
  present, no `$2`) is `PlannerError::UndefinedParameter { index: 2 }`,
  the same variant `Binder` raises for an unsubstituted placeholder,
  since both mean "this index has no value" from the caller's
  perspective. Unlike `Binder`, an unresolved column reference used to
  give a placeholder its type is not itself an error here - the type is
  simply left `None` - since this function's job is best-effort type
  inference for `Describe`, not full validation; a genuinely unknown
  table or column still surfaces as `UnknownTable`/`UnknownColumn` since
  those are needed to resolve the table's schema at all.
- `substitute_parameters(statement, values) -> Result<sql::Statement, PlannerError>` -
  replaces every `sql::Expr::Parameter { index }` in `statement` with
  `sql::Expr::Literal(values[index - 1].clone())`, returning the
  rewritten statement ready for `Binder::bind`. `expected` is the highest
  `$n` index actually referenced anywhere in `statement` (`0` if there are
  none); a `values.len()` that doesn't match it is
  `PlannerError::ParameterCountMismatch { expected, found: values.len() }`,
  checked before any substitution runs so an out-of-range index can never
  panic indexing into `values`. A numbering gap (`$1`/`$3` with no `$2`)
  is not itself rejected here - only `$3` needs a third value, and `$2`
  simply never gets substituted since nothing in the tree names it.
- `finalize(types) -> Result<Vec<Option<DataType>>, PlannerError>` -
  private. Turns the sparse `$n -> Option<DataType>` map
  `infer_parameter_types` built into the dense, `0`-indexed vector its
  signature promises, or `UndefinedParameter` for the first numbering gap
  found scanning `1..=max`.
- `record(types, index, data_type)` - private. Inserts `index`'s inferred
  type into the map if it has none yet; never downgrades an
  already-inferred `Some` back to `None` when the same placeholder is
  encountered a second time in a context that gives no type of its own
  (e.g. `WHERE a = $1 AND $1 = $1`).
- `collect_statement`/`collect_select`/`collect_insert` - private. Mirror
  `binder::Binder::bind`/`bind_select`/`bind_insert`'s statement-shape
  dispatch and table/column-list resolution, but recording placeholder
  context into `types` instead of producing a `BoundStatement`.
- `collect_expr(expr, schema, table_scope, types) -> Result<(), PlannerError>` -
  private. Walks one expression, recording a bare placeholder as `None`
  and delegating a `BinaryOp` to `collect_binary`; `schema`/`table_scope`
  mirror `Binder::bind_expr`'s own parameters, `None` where there is no
  table in scope (as inside `INSERT ... VALUES`, or in a `SELECT` with no
  `FROM` clause, where `collect_select` resolves no table at all).
- `collect_binary(left, right, schema, table_scope, types) -> Result<(), PlannerError>` -
  private. For each side that is a bare placeholder, infers its type from
  the *other* side via `column_type` rather than recursing into it; a
  non-placeholder side recurses through `collect_expr` as usual, so a
  placeholder nested inside a larger expression on one side of a
  comparison is still found.
- `column_type(expr, schema, table_scope) -> Option<DataType>` - private.
  The type-inference counterpart of `Binder::bind_expr`'s column
  resolution: `Some` for a `Column` reference that resolves against
  `schema` (checked against `table_scope` exactly as the binder checks a
  qualifier), `None` for anything else, including an unresolved column -
  this function never errors, since an unresolved reference here just
  means "no type available," not "reject the statement."
- `max_parameter_index(statement) -> u32` / `scan_statement` / `scan_expr` -
  private. The read-only walk `substitute_parameters` uses to compute
  `expected` before consuming `statement` for rewriting.
- `substitute_statement`/`substitute_select`/`substitute_insert`/`substitute_expr` -
  private. Consumes `statement`, rebuilding it with every
  `Expr::Parameter` replaced by the corresponding `Expr::Literal`;
  `CreateTable`/`CreateIndex`/`Begin`/`Commit`/`Rollback` carry no
  expressions and pass through unchanged.

## Usage Example

```rust
use planner::{infer_parameter_types, substitute_parameters};

let param_types = infer_parameter_types(&statement, &catalog)?;
let substituted = substitute_parameters(statement, &[types::Value::Integer(1)])?;
let bound =
    planner::Binder::new(&catalog, planner::SessionContext::new("example")).bind(substituted)?;
```

`engine::Database::describe` calls `infer_parameter_types` to answer a
wire-protocol `Describe` without executing anything, and
`Database::execute_with_params` calls `substitute_parameters` before
handing the result to `Binder::bind` exactly as `execute` hands its
literal-only statement today - see `binder.MD` for why the binder itself
is never handed an unsubstituted placeholder.
