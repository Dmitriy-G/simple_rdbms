# lib

SQL front end: turns SQL text into an AST the `planner` can bind.

The pipeline is the classic two stages: `Lexer` turns source text into a
flat `Token` stream, and `Parser` is a hand-written recursive-descent
parser that turns that stream into a `Statement`. There is deliberately no
parser-generator or third-party SQL grammar here — writing the grammar by
hand is the point of this crate.

## Key Components

- `Lexer` - turns SQL source text into a token stream. See `lexer.MD`.
- `Parser` - turns a token stream into a `Statement` AST. See `parser.MD`.
- `Token`, `TokenKind` - one lexical token and its kind. See `token.MD`.
- `Statement`, `SelectStatement`, `SelectItem`, `InsertStatement`,
  `CreateTableStatement`, `ColumnDef`, `Expr`, `BinaryOperator`,
  `UnaryOperator` - the parsed AST types. See `ast.MD`.
- `SqlError` - errors raised while lexing or parsing. See `error.MD`.

## Usage Example

```rust
use sql::{Lexer, Parser};

let tokens = Lexer::new("SELECT * FROM t WHERE a = 1").tokenize()?;
let statement = Parser::new(tokens).parse()?;
```

`engine::Database::execute` runs exactly this two-step pipeline against
each statement of SQL text it's given, rendering any `SqlError` against
the original source (via `SqlError::render`) before wrapping it as
`common::Error::Parse`.
