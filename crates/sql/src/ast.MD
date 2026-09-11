# ast

The AST types `Parser` produces: a single parsed SQL statement and the
expressions it's built from. The unit of work the `planner`'s binder
consumes.

## Key Components

- `Statement` - a single parsed SQL statement. `Select(SelectStatement)`
  is a `SELECT` query. `Insert(InsertStatement)` is an
  `INSERT INTO ... VALUES ...` statement. `CreateTable(CreateTableStatement)`
  is a `CREATE TABLE` statement. `CreateIndex(CreateIndexStatement)` is a
  `CREATE INDEX` statement. `Begin` is `BEGIN` or `START TRANSACTION`:
  starts an explicit transaction. `Commit` ends the current explicit
  transaction, keeping its writes. `Rollback` ends the current explicit
  transaction, undoing its writes. `Explain { verbose, inner }` is an
  `EXPLAIN [VERBOSE] <statement>`: `inner` is the statement to explain,
  restricted by `Parser::parse_explain` to whatever `planner::plan` can
  handle (`Select`, `Insert`, `CreateTable`, `CreateIndex`) - a
  transaction-control statement or another `Explain` as the target is a
  parse error, not something that reaches this variant.
- `SelectStatement` - a `SELECT` query: `SELECT <items> [FROM <table>
  [[AS] alias]] [WHERE <expr>]`. No joins, `GROUP BY`, or `ORDER BY` yet.
  `items` are the projected items, in output order. `from` is the
  optional single source table, with its optional alias; `None` is a
  select list of pure expressions - `SELECT 1`, `SELECT 'keep alive'` -
  which the binder resolves in an empty column scope and the planner
  runs over a `LogicalPlan::OneRow` source. `where_clause` is the
  optional filter predicate, and it is allowed with `from: None`, as in
  PostgreSQL, where it filters that single anonymous row.
- `TableRef` - the single source table of a `FROM` clause: `name` is the
  table as it exists in the catalog; `alias` is the optional `[AS]
  alias` naming a range variable for it. This is the one place `M23.1`'s
  eventual multi-table `FROM` list will extend from - see
  `planner::binder.MD` for why an alias, once given, replaces `name` as
  the only valid qualifier for `Expr::Column { table, .. }` inside this
  statement, rather than both being accepted.
- `SelectItem` - one item in a `SELECT` list: `Wildcard` (`*`, every
  column of the source table) or `Expr(Expr)` (a single projected
  expression).
- `InsertStatement` - an `INSERT INTO <table> [(<columns>)] VALUES
  (<row>), ...` statement. `table` is the destination table's name.
  `columns` are the target columns, in the order values are given; empty
  means "all columns, in schema order". `values` are the rows being
  inserted, each a list of value expressions aligned with `columns`.
- `CreateTableStatement` - a `CREATE TABLE <table> (<columns>)` statement.
  `table` is the new table's name; `columns` are its column definitions,
  in declared order.
- `CreateIndexStatement` - a `CREATE INDEX <index_name> ON <table>
  (<column>)` statement: single-column only, no `UNIQUE` or expression
  indexes. `index_name` is the new index's name; `table` and `column` name
  the column it covers.
- `ColumnDef` - one column definition inside a `CREATE TABLE` statement:
  `name`, declared `data_type`, and `nullable` (whether the column allows
  `NULL`; true unless `NOT NULL` was given — though the current grammar
  never produces `NOT NULL`, so this is always `true` in practice).
- `Expr` - a scalar SQL expression, as parsed (not yet bound to a schema).
  `Literal(Value)` is a literal value. `Column { table, name }` is a
  column reference: `name` alone (`table: None`) for a bare `col`, or
  `table` set for a qualified `t.col` - `table` names a range variable
  (a `TableRef`'s `name` or `alias`), not necessarily the literal table
  name once an alias is in play (`planner::binder.MD`). `BinaryOp { left,
  op, right }` applies a `BinaryOperator` to two operands. `UnaryOp { op,
  expr }` applies a `UnaryOperator` to one operand. `IsNull { expr, negated }` is `<expr> IS NULL` (`negated`
  false) or `<expr> IS NOT NULL` (`negated` true) - the only way to test
  for `NULL` that actually returns a `Boolean`: `<expr> = NULL` parses as
  an ordinary `BinaryOp` and, under three-valued logic, evaluates to
  `NULL` for every row rather than ever selecting one (`expression.MD` in
  `executor`). `Parameter { index }` is a `$n` placeholder from a prepared
  statement, 1-based to match the wire protocol's own numbering
  (`token.MD`'s `TokenKind::Parameter`); it is accepted anywhere a literal
  is, and `planner::binder::Binder` rejects one directly - reaching the
  binder unsubstituted means the statement was run without its parameter
  values, since `planner::parameters::substitute_parameters` replaces
  every `Parameter` with a `Literal` first.
  `SessionContext(SessionContextName)` is one of the fixed
  session-context values below, recognized by the parser rather than
  resolved as a name.
- `SessionContextName` - the closed set of session-context expressions
  this engine answers: `CurrentCatalog`, `CurrentSchema`, `CurrentUser`,
  `SessionUser`, `Version`. It is deliberately **not** a function
  registry; `planner::binder` replaces each with a value at bind time,
  and a general scalar-function subsystem - a function catalog,
  resolution by name and argument types, an evaluator - belongs to
  `docs/ROADMAP.md`'s M20 rather than to this enum. The test for whether
  a new name belongs here is in
  `docs/adr/0022-session-context-expressions.md`: a value the connection
  knows about itself, with no arguments and no per-row variation, may
  join the list; anything else waits for M20. An identifier that is
  *not* on this list and *is* followed by `(` is
  `SqlError::UndefinedFunction`, so `now()` reports what is actually
  missing instead of failing as a syntax error.
- `SessionContextName::keyword(word) -> Option<Self>` - the spellings SQL
  writes as niladic *keywords*, matched case-insensitively and valid with
  no parentheses: `current_catalog`, `current_schema`, `current_user`,
  `session_user` and `user` (the last being another spelling of
  `current_user`, as in PostgreSQL).
- `SessionContextName::function(word) -> Option<Self>` - the spellings
  valid only in call position, `<name>()`: `current_database` (the same
  value as `current_catalog`) and `version`, plus every keyword spelling,
  since `current_schema()` is as ordinary as `current_schema`. The split
  matters because a bare `version` or `current_database` must stay an
  ordinary column reference - a user table may own a column with that
  name, and only the parenthesized form can be the session-context value.
- `SessionContextName::column_name(self) -> &'static str` - the result
  column's name, which is what a driver reads these back by
  (`current_catalog`, `current_schema`, `current_user`, `session_user`,
  `version`). `planner::explain::projected_column` uses it instead of the
  positional `columnN` every other expression gets, since the grammar has
  no select-list alias to name one with.
- `BinaryOperator` - a binary operator that can appear in an
  `Expr::BinaryOp`: `Eq` (`=`), `NotEq` (`<>`/`!=`), `Lt` (`<`), `LtEq`
  (`<=`), `Gt` (`>`), `GtEq` (`>=`), `And` (`AND`), `Or` (`OR`), `Plus`
  (`+`), `Minus` (`-`), `Multiply` (`*`), `Divide` (`/`).
- `UnaryOperator` - a unary operator that can appear in an
  `Expr::UnaryOp`: `Not` (`NOT`), `Negate` (unary `-`).

## Usage Example

```rust
use sql::{BinaryOperator, Expr};
use types::Value;

let predicate = Expr::BinaryOp {
    left: Box::new(Expr::Column { table: None, name: "a".to_string() }),
    op: BinaryOperator::Eq,
    right: Box::new(Expr::Literal(Value::BigInt(1))),
};
```

`Parser::parse` returns exactly one `Statement`; the `planner`'s binder
walks it (matching on `Statement`'s variant, then recursing through
`Expr`) to resolve table and column names against the `catalog` and
produce a bound logical plan.
