# token

The lexical tokens `Lexer` produces and `Parser` consumes.

## Key Components

- `TokenKind` - the kind of a lexical token, carrying any literal value
  the token represents. Keyword variants (`Select`, `From`, `Where`,
  `Insert`, `Into`, `Values`, `As`, `Create`, `Table`, `Index`, `On`,
  `And`, `Or`, `Not`, `Is`, `Null`, `True`, `False`, `Begin`, `Start` (of
  `START TRANSACTION`), `Transaction`, `Commit`, `Rollback`, `Explain`)
  carry no payload. `Index`/`On` exist solely for `CREATE INDEX ident ON
  ident (ident)` (`parser.MD`'s `parse_create_index`) - `On` is not reused
  for `JOIN ... ON`, since no `JOIN` grammar exists yet (M11). `Is` exists
  solely for `<expr> IS [NOT] NULL` (`parser.MD`'s `parse_is_null_operand`).
  `As` exists for `FROM table [AS] alias` (`parser.MD`'s
  `parse_optional_table_alias`) - the alias is equally valid without it,
  so `As` is a hint rather than something the grammar requires. `Explain`
  is `EXPLAIN` - `VERBOSE` deliberately has no variant of its own here:
  it is recognized off a plain `Identifier` token by the parser, only in
  the one grammar position right after `Explain`, so `verbose` still
  lexes as an ordinary identifier everywhere else (`parser.MD`'s
  `consume_verbose_keyword`). `Identifier(String)`
  is a bare identifier: a table, column, alias, or type name. `IntegerLiteral(i64)`
  is an integer literal, before it is narrowed to a `types::DataType`.
  `FloatLiteral(f64)` is a floating-point literal. `StringLiteral(String)`
  is a single-quoted string literal, with quoting already stripped.
  Punctuation/operator variants (`Comma`, `Semicolon`, `LParen`, `RParen`,
  `Star`, `Dot`, `Plus`, `Minus`, `Slash`, `Eq`, `NotEq`, `Lt`, `LtEq`,
  `Gt`, `GtEq`) mirror their source spelling. `Dot` doubles as the
  separator in a qualified column reference (`t.col`,
  `parser.MD`'s `parse_primary`) alongside its original role between an
  integer and fractional digits in a float literal (`lexer.MD`'s
  `lex_number`, which never emits a `Dot` token itself). `Eof` is always
  the final token in a token stream.
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
