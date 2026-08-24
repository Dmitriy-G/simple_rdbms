# lexer

Turns SQL source text into a flat stream of `Token`s. Holds no knowledge
of grammar; a `Parser` is what gives the token stream structure.

## Key Components

- `Lexer` - the lexer. Holds the source text, its bytes (for
  byte-oriented scanning), and a cursor `offset`.
- `Lexer::new(input) -> Self` - creates a lexer over `input`.
- `Lexer::tokenize(self) -> Result<Vec<Token>, SqlError>` - consumes the
  lexer, producing every token in `input` followed by a trailing
  `TokenKind::Eof`.
- `Lexer::peek_byte(&self) -> Option<u8>` - private. The byte at the
  current cursor, if any.
- `Lexer::peek_byte_at(&self, delta) -> Option<u8>` - private. The byte
  `delta` positions ahead of the cursor, if any.
- `Lexer::skip_trivia(&mut self)` - private. Advances past whitespace and
  `--` line comments.
- `Lexer::lex_number(&mut self) -> Result<TokenKind, SqlError>` - private.
  Scans a run of digits, optionally followed by a `.` and more digits,
  producing an `IntegerLiteral` or `FloatLiteral` depending on whether a
  fractional part was present; a literal that doesn't fit its target type
  becomes `SqlError::InvalidNumericLiteral` rather than being silently
  truncated.
- `Lexer::lex_string(&mut self, start) -> Result<TokenKind, SqlError>` -
  private. Scans a single-quoted string literal starting at the opening
  quote, with `''` treated as an escaped single quote.
- `Lexer::lex_ident_or_keyword(&mut self) -> TokenKind` - private. Scans
  an identifier, matching it case-insensitively against the keyword set;
  anything that doesn't match a keyword becomes an `Identifier` carrying
  the original (case-sensitive) spelling.
- `Lexer::lex_punct(&mut self, start) -> Result<TokenKind, SqlError>` -
  private. Scans one punctuation or operator token, including the
  two-character lookahead needed for `<=`, `<>`, `>=`, and `!=`.
- `Lexer::advance_while(&mut self, pred)` - private. Advances the cursor
  while `pred` holds on the current byte.
- `Lexer::char_at(&self, offset) -> char` - private. Decodes the UTF-8
  character starting at byte `offset`.

## Usage Example

```rust
use sql::Lexer;

let tokens = Lexer::new("SELECT * FROM t").tokenize()?;
```

`Parser::new` takes exactly the `Vec<Token>` `Lexer::tokenize` produces;
`engine::Database::execute` calls `Lexer::new(sql).tokenize()` as the
first step of running any SQL statement, rendering an `Err` against the
original source before it ever reaches the parser.
