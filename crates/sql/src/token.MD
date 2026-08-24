# token

The lexical tokens `Lexer` produces and `Parser` consumes.

## Key Components

- `TokenKind` - the kind of a lexical token, carrying any literal value
  the token represents. Keyword variants (`Select`, `From`, `Where`,
  `Insert`, `Into`, `Values`, `Create`, `Table`, `And`, `Or`, `Not`,
  `Null`, `True`, `False`, `Begin`, `Start` (of `START TRANSACTION`),
  `Transaction`, `Commit`, `Rollback`) carry no payload. `Identifier(String)`
  is a bare identifier: a table, column, or type name. `IntegerLiteral(i64)`
  is an integer literal, before it is narrowed to a `types::DataType`.
  `FloatLiteral(f64)` is a floating-point literal. `StringLiteral(String)`
  is a single-quoted string literal, with quoting already stripped.
  Punctuation/operator variants (`Comma`, `Semicolon`, `LParen`, `RParen`,
  `Star`, `Dot`, `Plus`, `Minus`, `Slash`, `Eq`, `NotEq`, `Lt`, `LtEq`,
  `Gt`, `GtEq`) mirror their source spelling. `Eof` is always the final
  token in a token stream.
- `Token` - one lexical token: its `kind` plus the `offset` (byte offset
  into the source text where it starts), used to report parse errors with
  a location.
- `Token::new(kind, offset) -> Self` - builds a token of `kind` starting
  at byte `offset`.

## Usage Example

```rust
use sql::{Token, TokenKind};

let token = Token::new(TokenKind::Select, 0);
```

`Lexer::tokenize` produces a `Vec<Token>` terminated by `TokenKind::Eof`;
`Parser` walks that vector with an internal cursor, matching on
`Token::kind` to decide which grammar production to descend into and
using `Token::offset` to locate any `SqlError` it raises.
