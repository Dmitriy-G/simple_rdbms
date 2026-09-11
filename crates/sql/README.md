# sql

SQL front end: turns SQL text into an AST the `planner` can bind.

## Architecture

`sql` sits just above `common`/`types` in the layered workspace (see
`docs/adr/0002-crate-splitting.md`) and depends on nothing that knows about
storage, the catalog, or execution — its whole job ends at producing a
`Statement`. The pipeline is the classic two stages: text -> `Lexer` ->
`Token` stream -> `Parser` -> `Statement` AST. There is deliberately no
parser-generator or third-party SQL grammar crate here (no `sqlparser`) —
writing the grammar by hand is the point of this crate. `Parser` is a
hand-written recursive-descent parser; expression parsing in particular
follows precedence climbing so that, e.g., `age > 20 AND age < 40 OR name =
'Carol'` parses with `AND` binding tighter than `OR`, matching SQL's usual
operator precedence.

## Key Components

- `lexer` - `Lexer`, turns SQL source text into a token stream. See
  [lexer.MD](src/lexer.MD).
- `parser` - `Parser`, turns a token stream into a `Statement` AST. See
  [parser.MD](src/parser.MD).
- `token` - `Token`, `TokenKind`: one lexical token and its kind. See
  [token.MD](src/token.MD).
- `ast` - `Statement`, `SelectStatement`, `SelectItem`, `InsertStatement`,
  `CreateTableStatement`, `ColumnDef`, `Expr`, `BinaryOperator`,
  `UnaryOperator`: the parsed AST types. See [ast.MD](src/ast.MD).
- `error` - `SqlError`, errors raised while lexing or parsing, renderable
  against the original source text via `SqlError::render`. See
  [error.MD](src/error.MD).
- `fingerprint` - `fingerprint`, redacts a statement's literal values so
  it's safe to log at `info`. See [fingerprint.MD](src/fingerprint.MD).

## Features

`CREATE TABLE`, `INSERT` (with one or more value tuples), `SELECT` (a
single table, `WHERE` with the usual comparison/boolean operators and
three-valued `NULL` logic), and `BEGIN`/`COMMIT`/`ROLLBACK` all parse today.
There is no `JOIN` syntax (the planner and executor have join scaffolding —
`LogicalPlan::Join`, `PhysicalPlan::NestedLoopJoin`,
`NestedLoopJoinExecutor` — but nothing in this crate's grammar can produce
a bound plan that reaches it), no `UPDATE`, `DELETE`, `DROP TABLE`,
`ALTER TABLE`, `GROUP BY`/`ORDER BY`/`LIMIT`, aggregate functions, or
subqueries.

There is also no function-call syntax, with one deliberate exception: a
closed list of **session-context expressions** — `current_catalog`,
`current_schema`, `current_user`, `session_user`, `user`,
`current_database()` and `version()` — parses as
`Expr::SessionContext`, which `planner`'s binder turns into a value
([ast.MD](src/ast.MD)'s `SessionContextName`). Every *other* identifier
followed by `(` is `SqlError::UndefinedFunction` (`42883`) rather than a
syntax error, so `now()` reports what is missing. That is an allowlist,
not a function registry: a real scalar-function subsystem is a milestone
of its own, and nothing here should grow towards one by adding names.

## Dependencies

Workspace: `common`, `types` (a parsed `ColumnDef`'s type names resolve to
`types::DataType`). External: `thiserror`, for `SqlError`.

## Configuration

None — `sql` has no configuration; `Lexer::new`/`Parser::new` take only the
input they operate on.

## Testing

`tests/parser_tests.rs` parses each statement form and asserts on the
resulting AST, checks operator precedence, and checks that malformed input
produces a located, renderable `SqlError`. `tests/smoke.rs` is the
minimum-viable compile-and-construct check. Run just this crate with:

```sh
cargo test -p sql
```
