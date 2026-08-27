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
  source table. `projections` are the projected expressions, resolved to
  column ordinals. `column_names` are display names for `projections`, in
  the same order: the source column's name for a bare column reference
  (including each column a `*` expands to), or a positional `columnN`
  name for any other expression, which has no name of its own to report.
  `predicate` is the optional, type-checked filter predicate.
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
  carrying its resolved result type.
- `Binder` - binds parsed `sql` AST nodes against a `Catalog`. Holds a
  reference to the catalog it resolves names against.
- `Binder::new(catalog) -> Self` - creates a binder that resolves names
  against `catalog`.
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
  private. Resolves the source table, expands `*` or binds each selected
  expression, and type-checks the optional `WHERE` predicate as `Boolean`.
- `Binder::bind_insert(&self, insert) -> Result<BoundInsert, PlannerError>` -
  private. Resolves the destination table and target column indices
  (every column, in schema order, if no explicit list was given), checks
  each row's value count against the target list, and binds each value
  against an empty schema — values in an `INSERT` have no source row to
  resolve column references against, so binding against an empty schema
  turns any bare column reference into an `UnknownColumn` error. Columns
  not covered by a row default to `BoundExpr::Literal(Value::Null)`.
- `Binder::bind_create_table(&self, create) -> BoundCreateTable` - private.
  Converts each parsed `ColumnDef` into a `BoundColumnDef` directly, with
  no catalog lookup (the table doesn't exist yet).
- `Binder::bind_create_index(&self, create) -> Result<BoundCreateIndex, PlannerError>` -
  private. Resolves `create.table` via `catalog.get_table` and
  `create.column` via `Schema::column_index`, the same patterns
  `bind_select`/`bind_insert` already use, raising `UnknownTable`/
  `UnknownColumn` for either miss.
- `Binder::bind_expr(&self, expr, schema) -> Result<(BoundExpr, Option<DataType>), PlannerError>` -
  private. Binds an expression against `schema`, resolving column
  references to ordinals and type-checking every operator application.
  Returns the bound expression along with its resolved type, or `None` if
  the expression is a `NULL` literal (which has no type of its own and is
  treated as compatible with any type it's checked against).
- `Binder::bind_unary_op(&self, op, expr, schema) -> Result<(BoundExpr, Option<DataType>), PlannerError>` -
  private. Type-checks `NOT` (operand must be `Boolean`) and unary `-`
  (operand must be numeric and non-`NULL`).
- `Binder::bind_binary_op(&self, left, op, right, schema) -> Result<(BoundExpr, Option<DataType>), PlannerError>` -
  private. Binds both operands, applies `coerce_comparison_operands` to
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

let bound = Binder::new(&catalog).bind(statement)?;
```

`engine::Database::execute` calls `Binder::new(&self.catalog).bind(statement)`
right after parsing, on the way to `plan::plan` and then `to_physical`;
`tests/binder_tests.rs` exercises every rejection path (unknown
table/column, type mismatches, column-count mismatches, out-of-range
literals) this module can raise.
