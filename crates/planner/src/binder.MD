# binder

Binds parsed `sql` AST nodes against a `Catalog`, producing a
`BoundStatement` with every name resolved and every expression
type-checked. This is the boundary where "syntactically valid SQL"
becomes "semantically valid against this database's schema".

## Key Components

- `BoundStatement` - a statement after binding: names resolved against
  the catalog and expressions type-checked, but still shaped like the SQL
  it came from rather than lowered to relational algebra. `Select`,
  `Insert`, `CreateTable`, and `CreateIndex` each wrap the corresponding
  bound struct. `Explain { verbose, inner }` wraps the bound form of
  whatever statement `EXPLAIN` targets - binding recurses into `inner`
  normally, so a missing table or column inside an `EXPLAIN`'d query
  fails exactly the same way it would unexplained; `engine::Database`
  unwraps this variant itself rather than ever handing it to
  `planner::plan` (`plan.MD`).
- `BoundSelect` - a `SELECT` after binding. `table_id` is the resolved
  source table, or `None` for a `SELECT` with no `FROM` clause, which
  `plan.MD` turns into a projection over `LogicalPlan::OneRow` instead of
  over a scan. `projections` are the projected expressions, resolved to
  column ordinals. `column_names` are display names for `projections`, in
  the same order: the source column's name for a bare column reference
  (including each column a `*` expands to), the expression's own spelling
  for a session-context expression (`current_schema`, `version`), or a
  positional `columnN` name for any other expression, which has no name
  of its own to report. The session-context case is named here *and* in
  `explain::projected_column`, which is the one duplication in this
  module worth stating outright: a client reads the result set's names
  from here and `EXPLAIN` reads them from there, and the two disagreeing
  is exactly the bug a driver would hit first.
  `column_types` is `bind_expr`'s resolved `Option<DataType>` for each
  entry of `projections`, in the same order - `Some` for every ordinary
  projection (a `*`-expanded column always carries its schema type), and
  `None` only for a projection with no static type of its own, such as a
  bare `NULL` literal. `engine::ResultSet::Rows` carries this straight
  through so a caller - `cli`'s formatter today, `server::wire`'s
  `RowDescription` from M13.2 onward - has a type to report for an empty
  result set, which has no rows to infer one from. `predicate` is the
  optional, type-checked filter predicate.
- `BoundInsert` - an `INSERT` after binding. `table_id` is the resolved
  destination table. `rows` are each row's values, one bound expression
  per column of the target table's schema (in schema order); columns not
  named in the statement's optional column list are bound to
  `Value::Null`.
- `BoundCreateTable` - a `CREATE TABLE` after binding (mainly a
  name-collision check; there is no schema to resolve against yet since
  the table doesn't exist). `table_name` and `columns` mirror the parsed
  statement.
- `BoundCreateIndex` - a `CREATE INDEX` after binding: `index_name` mirrors
  the parsed statement, `table_id` and `column_index` are the resolved
  target table and column ordinal - the same resolution `Schema::column_index`
  does for any other column reference, catching an unknown table or column
  before `catalog::Catalog::create_index` ever runs. Unlike
  `BoundCreateTable`, there *is* a schema to resolve against, since the
  table must already exist for `CREATE INDEX ... ON` to name it.
- `BoundColumnDef` - a column definition after binding, e.g. after
  validating that its type is supported and its name does not repeat
  within the same statement. `name`, `data_type`, and `nullable` mirror
  the parsed column definition.
- `BoundExpr` - a scalar expression after binding: column references are
  resolved to ordinal positions and literal/operator types have been
  checked. `Literal(Value)` is a literal value. `ColumnRef { index,
  data_type }` references the column at ordinal `index` in the bound
  source's schema, with `data_type` cached to avoid re-deriving it during
  execution. `BinaryOp { left, op, right, data_type }` and `UnaryOp { op,
  expr, data_type }` mirror the parsed operator applications, each
  carrying its resolved result type. `IsNull { expr, negated }` mirrors
  `sql::Expr::IsNull` - its result type is always `Boolean`, so unlike the
  other compound variants it carries no `data_type` field of its own.
  `SessionContext { name, value }` is a session-context expression
  (`sql::SessionContextName`) already resolved to the `Value` it will
  produce - the binder is the last layer that knows these are anything
  but a constant. It is a variant of its own rather than a plain
  `Literal` for one reason: `explain::projected_column` names the result
  column after `name`, and a driver reads `current_schema` back by that
  name, which a `Literal` would have had to call `columnN`.
- `SessionContext` - the per-database values a session-context expression
  resolves to: `database`, `schema`, `user` and `server_version`. It is
  built once, when the engine opens, from `common::DbConfig` - the
  database is `DbConfig::database_name()`, the schema is always `public`
  and the user is always `DbConfig::FIXED_USER_NAME`, since nothing
  authenticates until M22
  (`docs/adr/0021-one-database-one-role-until-m22-and-m24.md`).
- `SessionContext::new(database) -> Self` - the whole constructor: the
  database name, and the three fixed values above.
- `Binder` - binds parsed `sql` AST nodes against a `Catalog`. Holds a
  reference to the catalog it resolves names against and the
  `SessionContext` a session-context expression evaluates from.
- `Binder::new(catalog, session) -> Self` - creates a binder that
  resolves names against `catalog` and session-context expressions
  against `session`. The session is a required argument rather than an
  optional one so there is no default that silently answers
  `current_catalog` with the wrong database.
- `Binder::bind(&self, statement) -> Result<BoundStatement, PlannerError>` -
  binds a parsed statement, resolving every table/column reference and
  type-checking every expression. Transaction-control statements
  (`Begin`/`Commit`/`Rollback`) reference no tables or columns and are
  intercepted in `Database::execute` before binding, so reaching this
  method with one is unreachable. `sql::Statement::Explain { verbose,
  inner }` recurses: calls `bind` on `inner` and wraps the result in
  `BoundStatement::Explain`, carrying `verbose` through unchanged - the
  parser already guarantees `inner` is never itself a transaction-control
  statement or another `Explain` (`sql::parser.MD`'s `parse_explain`), so
  this recursive call can never hit the unreachable arm above.
- `Binder::bind_select(&self, select) -> Result<BoundSelect, PlannerError>` -
  private. Resolves the source table by `select.from`'s `name` (always the
  real catalog name, never the alias), expands `*` or binds each selected
  expression, and type-checks the optional `WHERE` predicate as `Boolean`.
  A `select.from` of `None` - a `SELECT` of pure expressions, such as a
  JDBC client's `SELECT 'keep alive'` - resolves no table at all and binds
  the select list and the predicate against an empty schema with
  `table_scope: None`, exactly as `bind_insert` binds its values, so a
  column reference in that position is an `UnknownColumn` rather than a
  silent `NULL`. Computes `table_scope` as the alias, falling back to the
  table name when there is no alias, and passes it to every
  `bind_expr` call so a qualified column reference (`sql::Expr::Column {
  table: Some(_), .. }`) can be checked against it. Once an alias is
  given, it - not the original table name - is the only valid qualifier;
  this mirrors Postgres, where an aliased table's original name goes out
  of scope for qualification inside that statement.
- `Binder::bind_insert(&self, insert) -> Result<BoundInsert, PlannerError>` -
  private. Resolves the destination table and target column indices
  (every column, in schema order, if no explicit list was given), checks
  each row's value count against the target list, and binds each value
  against an empty schema with no table scope (`table_scope: None`) —
  values in an `INSERT` have no source row to resolve column references
  against, so a bare column reference becomes `UnknownColumn` and a
  qualified one becomes `UnknownTable` (nothing is in scope to qualify
  against). Columns not covered by a row default to
  `BoundExpr::Literal(Value::Null)`.
- `Binder::bind_create_table(&self, create) -> BoundCreateTable` - private.
  Converts each parsed `ColumnDef` into a `BoundColumnDef` directly, with
  no catalog lookup (the table doesn't exist yet).
- `Binder::bind_create_index(&self, create) -> Result<BoundCreateIndex, PlannerError>` -
  private. Resolves `create.table` via `catalog.get_table` and
  `create.column` via `Schema::column_index`, the same patterns
  `bind_select`/`bind_insert` already use, raising `UnknownTable`/
  `UnknownColumn` for either miss.
- `Binder::bind_expr(&self, expr, schema, table_scope) -> Result<(BoundExpr, Option<DataType>), PlannerError>` -
  private. Binds an expression against `schema`, resolving column
  references to ordinals and type-checking every operator application.
  `table_scope` is the one range-variable name (a `TableRef`'s alias or
  name; `None` where there is no table in scope at all, as in
  `bind_insert`) a qualified column reference is allowed to name -
  `sql::Expr::Column { table: Some(qualifier), name }` is rejected with
  `PlannerError::UnknownTable(qualifier)` unless `table_scope ==
  Some(qualifier)`; `table: None` resolves `name` against `schema`
  exactly as before, regardless of `table_scope`. `table_scope` is
  threaded unchanged through every recursive call - it names the one
  table a qualifier may refer to, not a type to check an operand against,
  so it never varies within one statement the way `schema` conceptually
  could once joins exist (M23.1). Returns the bound expression along with
  its resolved type, or `None` if the expression is a `NULL` literal
  (which has no type of its own and is treated as compatible with any
  type it's checked against). `sql::Expr::IsNull { expr, negated }` binds
  its operand against no type restriction at all (any type, including an
  untyped `NULL` literal, can be tested for nullness) and always resolves
  to `Boolean`. `sql::Expr::Parameter { index }` is rejected outright with
  `PlannerError::UndefinedParameter { index }` - this binder never
  substitutes a placeholder itself, so reaching one here means the
  statement was bound without `parameters::substitute_parameters` having
  replaced every `$n` with a literal first (`parameters.MD`).
- `Binder::bind_unary_op(&self, op, expr, schema, table_scope) -> Result<(BoundExpr, Option<DataType>), PlannerError>` -
  private. Type-checks `NOT` (operand must be `Boolean`) and unary `-`
  (operand must be numeric and non-`NULL`); `table_scope` passes straight
  through to the recursive `bind_expr` call.
- `Binder::bind_binary_op(&self, left, op, right, schema, table_scope) -> Result<(BoundExpr, Option<DataType>), PlannerError>` -
  private. Binds both operands (each against the same `table_scope`),
  applies `coerce_comparison_operands` to
  let a literal narrow towards a compared column's type, then type-checks
  by operator family: `AND`/`OR` require `Boolean` operands;
  `=`/`<>`/`<`/`<=`/`>`/`>=` require matching operand types; arithmetic
  operators (`+`/`-`/`*`/`/`) are rejected outright as unsupported.
- `is_numeric(data_type) -> bool` - private. Whether `data_type` is a
  numeric type eligible for unary negation.
- `data_types_match(a, b) -> bool` - private. Whether two resolved types
  may appear together in a comparison or an assignment, ignoring a
  `Varchar`'s declared maximum length (an identity property of the
  column, not of the value being compared against it). This is
  deliberately asymmetric in spirit even though the check itself is
  symmetric: literals adapt to columns (see `coerce_literal_to_column`,
  applied before this check ever runs), but two column references never
  adapt to each other. By the time a comparison reaches this function,
  either operand still carrying a type that differs from the other's is a
  genuine mismatch, not something coercion should paper over.
- `coerce_literal_to_column(value, target, column_name) -> Result<Value, PlannerError>` -
  private. If `value` is a literal whose type differs from `target` but
  can be exactly represented in it, narrows/widens it to `target`;
  otherwise returns `value` unchanged so the caller's normal type-mismatch
  check applies. Only a `BigInt` literal against an `Integer` target is
  coerced today (a literal integer that overflows `i32` in an `Integer`
  column), reported against `column_name` if it doesn't fit.
- `coerce_comparison_operands(left, left_type, right, right_type, schema) -> Result<(BoundExpr, Option<DataType>, BoundExpr, Option<DataType>), PlannerError>` -
  private. Applies literal-to-column coercion to a comparison's operands:
  when one side is a bound literal and the other a column reference, the
  literal is coerced towards the column's type before the comparison's own
  type check runs. Column-to-column comparisons are returned unchanged.

## Usage Example

```rust
use planner::Binder;

use planner::SessionContext;

let bound = Binder::new(&catalog, SessionContext::new("example")).bind(statement)?;
```

`engine::Database::execute` calls
`Binder::new(&self.catalog, self.session_context.clone()).bind(statement)`
right after parsing, on the way to `plan::plan` and then `to_physical`;
`tests/binder_tests.rs` exercises every rejection path (unknown
table/column, type mismatches, column-count mismatches, out-of-range
literals) this module can raise.
