# ast

The AST types `Parser` produces: a single parsed SQL statement and the
expressions it's built from. The unit of work the `planner`'s binder
consumes.

## Key Components

- `Statement` - a single parsed SQL statement. `Select(SelectStatement)`
  is a `SELECT` query. `Insert(InsertStatement)` is an
  `INSERT INTO ... VALUES ...` statement. `CreateTable(CreateTableStatement)`
  is a `CREATE TABLE` statement. `Begin` is `BEGIN` or `START TRANSACTION`:
  starts an explicit transaction. `Commit` ends the current explicit
  transaction, keeping its writes. `Rollback` ends the current explicit
  transaction, undoing its writes.
- `SelectStatement` - a `SELECT` query: `SELECT <items> FROM <table>
  [WHERE <expr>]`. No joins, `GROUP BY`, or `ORDER BY` yet. `items` are the
  projected items, in output order. `from` is the single source table's
  name. `where_clause` is the optional filter predicate.
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
- `ColumnDef` - one column definition inside a `CREATE TABLE` statement:
  `name`, declared `data_type`, and `nullable` (whether the column allows
  `NULL`; true unless `NOT NULL` was given — though the current grammar
  never produces `NOT NULL`, so this is always `true` in practice).
- `Expr` - a scalar SQL expression, as parsed (not yet bound to a schema).
  `Literal(Value)` is a literal value. `Column(String)` is a bare column
  reference. `BinaryOp { left, op, right }` applies a `BinaryOperator` to
  two operands. `UnaryOp { op, expr }` applies a `UnaryOperator` to one
  operand.
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
    left: Box::new(Expr::Column("a".to_string())),
    op: BinaryOperator::Eq,
    right: Box::new(Expr::Literal(Value::BigInt(1))),
};
```

`Parser::parse` returns exactly one `Statement`; the `planner`'s binder
walks it (matching on `Statement`'s variant, then recursing through
`Expr`) to resolve table and column names against the `catalog` and
produce a bound logical plan.
