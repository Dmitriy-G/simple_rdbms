# parser

A hand-written recursive-descent parser over a `Token` stream, producing a
`Statement` AST. Each grammar production gets its own method, named after
the production it parses, and consumes tokens by advancing an internal
cursor rather than by recursing over the input text.

## Key Components

- `Parser` - the parser. Holds the complete token stream and a cursor
  `pos` into it.
- `Parser::new(tokens) -> Self` - creates a parser over a complete token
  stream (as produced by `Lexer::tokenize`).
- `Parser::parse(&mut self) -> Result<Statement, SqlError>` - parses
  exactly one statement via `parse_statement`, terminated by `;` or end of
  input.
- `Parser::parse_statement(&mut self) -> Result<Statement, SqlError>` -
  private. Dispatches on the leading keyword to `parse_select`,
  `parse_insert`, `parse_create_table`/`parse_create_index`, one of the
  transaction-control keywords (`BEGIN`, `START TRANSACTION`, `COMMIT`,
  `ROLLBACK`), or `parse_explain`. `CREATE` alone doesn't say which:
  dispatch peeks one token ahead (`peek_kind(1)`) to see `TABLE` or
  `INDEX` before committing to either sub-parser, since both
  `parse_create_table` and `parse_create_index` re-consume the `CREATE`
  token themselves rather than being handed a stream with it already
  stripped. Also the recursive base case `parse_explain` calls to parse
  its inner statement, so an `EXPLAIN`'s target goes through the exact
  same dispatch as a top-level statement.
- `Parser::parse_explain(&mut self) -> Result<Statement, SqlError>` -
  private. Consumes `EXPLAIN`, then `consume_verbose_keyword` for an
  optional `VERBOSE`, then recurses into `parse_statement` for the target.
  Rejects the result with `SqlError::ExplainOfTransactionControl` if the
  target parsed as `Begin`/`Commit`/`Rollback` (they have no plan to
  explain) or `SqlError::NestedExplain` if it parsed as another `Explain`
  (caught this way rather than by pre-checking the leading token, since
  the recursive `parse_statement` call is what actually produces the
  nested `Statement::Explain` to reject) - both checks run after the
  recursive parse succeeds, so the error always points at the *outer*
  `EXPLAIN`'s offset, not the inner one.
- `Parser::consume_verbose_keyword(&mut self) -> bool` - private. The only
  place `VERBOSE` is recognized at all: if the current token is an
  `Identifier` spelled `VERBOSE` (case-insensitively), consumes it and
  returns `true`; otherwise leaves the cursor untouched and returns
  `false`. Called only from `parse_explain`, immediately after `EXPLAIN` -
  `VERBOSE` has no `TokenKind` of its own (`token.MD`), so this is the
  only grammar position where the word carries special meaning; anywhere
  else, including `SELECT verbose FROM t`, it lexes and parses as a plain
  column reference.
- `Parser::parse_select(&mut self) -> Result<SelectStatement, SqlError>` -
  private. Parses a `SELECT` statement.
- `Parser::parse_select_list(&mut self) -> Result<Vec<SelectItem>, SqlError>` -
  private. Parses a `select_list`: either a bare `*` or a comma-separated
  list of expressions.
- `Parser::parse_table_ref(&mut self) -> Result<TableRef, SqlError>` -
  private. Parses a `FROM` clause's single source table: a required table
  name followed by `parse_optional_table_alias`.
- `Parser::parse_optional_table_alias(&mut self) -> Result<Option<String>, SqlError>` -
  private. Consumes `AS ident` if the current token is `AS`; otherwise, if
  the current token is a bare `Identifier` (not `WHERE`, `;`, or `EOF` -
  none of which lex as `Identifier`), consumes it as an alias with no
  `AS`; otherwise leaves the cursor untouched and returns `None`. Mirrors
  Postgres's optional-`AS` table alias grammar.
- `Parser::parse_insert(&mut self) -> Result<InsertStatement, SqlError>` -
  private. Parses an `INSERT INTO` statement.
- `Parser::parse_value_row(&mut self) -> Result<Vec<Expr>, SqlError>` -
  private. Parses one parenthesized `expr_list` of a `VALUES` clause.
- `Parser::parse_create_table(&mut self) -> Result<CreateTableStatement, SqlError>` -
  private. Parses a `CREATE TABLE` statement.
- `Parser::parse_create_index(&mut self) -> Result<CreateIndexStatement, SqlError>` -
  private. Parses a `CREATE INDEX <index_name> ON <table> (<column>)`
  statement - single column, no `UNIQUE`.
- `Parser::parse_col_def(&mut self) -> Result<ColumnDef, SqlError>` -
  private. Parses one `col_def`: an identifier followed by a type name.
  The grammar has no `NOT NULL` modifier, so every column is nullable.
- `Parser::parse_type_name(&mut self) -> Result<DataType, SqlError>` -
  private. Parses a `type_name`: `INTEGER | INT | BIGINT | DOUBLE | TEXT |
  BOOLEAN`, matched case-insensitively against an identifier token.
- `Parser::parse_expr(&mut self) -> Result<Expr, SqlError>` - private.
  Parses an expression, respecting operator precedence and associativity
  via precedence climbing: `OR` binds loosest, then `AND`, then
  comparisons, then postfix `IS [NOT] NULL`, then unary `NOT`/`-`, then
  primaries. Delegates to `parse_or`.
- `Parser::parse_or`, `parse_and`, `parse_comparison`, `parse_unary`,
  `parse_primary` - private. The precedence-climbing ladder `parse_expr`
  sits on top of, each one level tighter than the last; `parse_primary`
  is the base case, matching literals, column references, and
  parenthesized sub-expressions - a bare identifier becomes
  `Expr::Column { table: None, name }`, and one immediately followed by
  `.` and a second identifier becomes `Expr::Column { table: Some(first),
  name: second }`, a qualified reference the binder resolves against the
  `FROM` clause's `TableRef` (`planner::binder.MD`). `parse_comparison` parses its operands
  through `parse_is_null_operand` rather than `parse_unary` directly, so
  `IS [NOT] NULL` binds to one comparison operand the same way a
  Postgres-style grammar treats it - tighter than `=`/`<`/etc, looser
  than unary `NOT`/`-`.
- `Parser::parse_is_null_operand(&mut self) -> Result<Expr, SqlError>` -
  private. Parses one `parse_unary` operand, then greedily wraps it in
  `Expr::IsNull { expr, negated }` for each trailing `IS NULL` (`negated:
  false`) or `IS NOT NULL` (`negated: true`) it finds - a loop rather than
  an `if`, so a (pathological but grammatically sound) repeated `x IS
  NULL IS NOT NULL` parses instead of leaving a trailing `IS` for the
  caller to choke on.
- `Parser::current(&self) -> &Token` - private. The token at the cursor.
- `Parser::peek_kind(&self, ahead) -> &TokenKind` - private. The kind of
  the token `ahead` positions past the cursor, clamped to the final token
  (`Eof`) rather than panicking if `ahead` runs past the stream's end -
  used by `parse` for its one-token `CREATE TABLE`/`CREATE INDEX`
  lookahead, without consuming anything.
- `Parser::advance(&mut self) -> Token` - private. Returns the current
  token and moves the cursor forward, unless it is already on the final
  token (`Eof`), which is never consumed.
- `Parser::expect_kind(&mut self, kind, expected) -> Result<Token, SqlError>` -
  private. Consumes the current token if its kind matches `kind` exactly,
  otherwise reports an error naming `expected`.
- `Parser::expect_identifier(&mut self) -> Result<String, SqlError>` -
  private. Consumes the current token if it is an `Identifier`, otherwise
  reports an error.
- `Parser::unexpected(&self, expected) -> SqlError` - private. Builds the
  right error variant (`UnexpectedEof` vs. `UnexpectedToken`) for the
  current token, naming what the grammar expected instead.

## Usage Example

```rust
use sql::{Lexer, Parser};

let tokens = Lexer::new("INSERT INTO t VALUES (1, 'x')").tokenize()?;
let statement = Parser::new(tokens).parse()?;
```

`engine::Database::execute` calls `Parser::new(tokens).parse()` right
after tokenizing, producing the single `Statement` that the `planner`'s
binder then resolves against the `catalog`.
