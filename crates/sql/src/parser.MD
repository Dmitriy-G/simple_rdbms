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
  exactly one statement, terminated by `;` or end of input; dispatches on
  the leading keyword to `parse_select`, `parse_insert`,
  `parse_create_table`, or one of the transaction-control keywords
  (`BEGIN`, `START TRANSACTION`, `COMMIT`, `ROLLBACK`).
- `Parser::parse_select(&mut self) -> Result<SelectStatement, SqlError>` -
  private. Parses a `SELECT` statement.
- `Parser::parse_select_list(&mut self) -> Result<Vec<SelectItem>, SqlError>` -
  private. Parses a `select_list`: either a bare `*` or a comma-separated
  list of expressions.
- `Parser::parse_insert(&mut self) -> Result<InsertStatement, SqlError>` -
  private. Parses an `INSERT INTO` statement.
- `Parser::parse_value_row(&mut self) -> Result<Vec<Expr>, SqlError>` -
  private. Parses one parenthesized `expr_list` of a `VALUES` clause.
- `Parser::parse_create_table(&mut self) -> Result<CreateTableStatement, SqlError>` -
  private. Parses a `CREATE TABLE` statement.
- `Parser::parse_col_def(&mut self) -> Result<ColumnDef, SqlError>` -
  private. Parses one `col_def`: an identifier followed by a type name.
  The grammar has no `NOT NULL` modifier, so every column is nullable.
- `Parser::parse_type_name(&mut self) -> Result<DataType, SqlError>` -
  private. Parses a `type_name`: `INTEGER | INT | BIGINT | DOUBLE | TEXT |
  BOOLEAN`, matched case-insensitively against an identifier token.
- `Parser::parse_expr(&mut self) -> Result<Expr, SqlError>` - private.
  Parses an expression, respecting operator precedence and associativity
  via precedence climbing: `OR` binds loosest, then `AND`, then
  comparisons, then unary `NOT`/`-`, then primaries. Delegates to
  `parse_or`.
- `Parser::parse_or`, `parse_and`, `parse_comparison`, `parse_unary`,
  `parse_primary` - private. The precedence-climbing ladder `parse_expr`
  sits on top of, each one level tighter than the last; `parse_primary`
  is the base case, matching literals, column references, and
  parenthesized sub-expressions.
- `Parser::current(&self) -> &Token` - private. The token at the cursor.
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
