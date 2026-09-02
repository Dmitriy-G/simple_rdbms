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
  list of expressions. No `AS alias` here yet - `SelectItem` has no alias
  field to put one in - even though `TokenKind::As` already exists for
  `parse_optional_table_alias`. `SELECT col AS alias` is M20's scope
  (`docs/ROADMAP.md`); until then `AS` in this position simply isn't
  recognized, and `SELECT col AS alias FROM t` is a parse error, not a
  silently-ignored alias.
- `Parser::parse_table_ref(&mut self) -> Result<TableRef, SqlError>` -
  private. Parses a `FROM` clause's single source table: a required table
  name followed by `parse_optional_table_alias`.
- `Parser::parse_optional_table_alias(&mut self) -> Result<Option<String>, SqlError>` -
  private. Consumes `AS ident` if the current token is `AS`; otherwise, if
  the current token is a bare `Identifier` that is not one of
  `lexer::RESERVED_WORDS` (`lexer.MD`), consumes it as an alias with no
  `AS`; otherwise leaves the cursor untouched and returns `None`. The
  reserved-word check is what stops `SELECT * FROM t ORDER BY x` from
  parsing `ORDER` as `t`'s alias and then failing on `BY` with a
  misleading message - `ORDER`, like every word in the reserved list,
  has no `TokenKind` of its own yet, so without the check it would look
  exactly like a legitimate alias. Declining leaves `ORDER` itself as the
  current token, so the statement still fails - `ORDER BY` isn't
  supported until M20 - but as an ordinary "unexpected token" error
  naming `ORDER`, not a confusing one about `BY`. Mirrors Postgres's
  optional-`AS` table alias grammar, including the reserved/unreserved
  keyword distinction: a word can be reserved for alias position without
  yet being a keyword anywhere else.
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
  via precedence climbing: `OR` binds loosest, then `AND`, then prefix
  `NOT`, then postfix `IS [NOT] NULL`, then comparisons, then unary `-`,
  then primaries. Delegates to `parse_or`.
- `Parser::parse_or`, `parse_and`, `parse_not`, `parse_is_null`,
  `parse_comparison`, `parse_unary`, `parse_primary` - private. The
  precedence-climbing ladder `parse_expr` sits on top of, each one level
  tighter than the last; `parse_primary` is the base case, matching
  literals, column references, and parenthesized sub-expressions - a bare
  identifier becomes `Expr::Column { table: None, name }`, and one
  immediately followed by `.` and a second identifier becomes
  `Expr::Column { table: Some(first), name: second }`, a qualified
  reference the binder resolves against the `FROM` clause's `TableRef`
  (`planner::binder.MD`). This ladder matches Postgres's order exactly,
  highest to lowest: comparison, `IS`, `NOT`, `AND`, `OR`, with unary
  minus kept separate and high (inside `parse_unary`, above the
  multiplicative/additive levels M15 will add) since Postgres binds unary
  minus above `*`/`/` rather than down at `NOT`'s level.
  `parse_is_null` sits between `parse_not` and `parse_comparison`, so
  `IS [NOT] NULL` applies to an entire comparison, not to one of its
  operands - `a = b IS NULL` parses as `(a = b) IS NULL`, matching
  Postgres, where `IS` sits below the comparison operators. This is
  deliberately *not* the naive placement (`IS` inside `parse_comparison`,
  next to `parse_unary`): that placement parses `a = b IS NULL` as `a =
  (b IS NULL)` instead, which then fails to bind (`IS NULL` is always
  `Boolean`, and `a` typically isn't) - a real bug this precedence fixes,
  not a style preference. The common forms (`WHERE col IS NULL`, `WHERE
  col IS NOT NULL AND x = 1`) parse identically either way, since neither
  involves `IS` applied to the result of a comparison, which is exactly
  why the wrong placement went unnoticed. `parse_not` sits between
  `parse_and` and `parse_is_null` for the same reason: `NOT` used to be
  handled in `parse_unary`, below `parse_comparison`, so `WHERE NOT a = 1`
  parsed as `(NOT a) = 1` and then failed to bind, since `NOT` requires a
  Boolean operand and `a` typically isn't one. Moving it above
  `parse_comparison` makes `WHERE NOT a = 1` parse as `NOT (a = 1)`,
  matching Postgres.
- `Parser::parse_not(&mut self) -> Result<Expr, SqlError>` - private. If
  the current token is `NOT`, consumes it and recurses on itself (so
  `NOT NOT a = 1` parses as `NOT (NOT (a = 1))`), wrapping the result in
  `Expr::UnaryOp { op: UnaryOperator::Not, expr }`; otherwise delegates
  straight to `parse_is_null`.
- `Parser::parse_is_null(&mut self) -> Result<Expr, SqlError>` - private.
  Parses one `parse_comparison` result, then greedily wraps it in
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
