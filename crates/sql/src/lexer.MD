# lexer

Turns SQL source text into a flat stream of `Token`s. Holds no knowledge
of grammar; a `Parser` is what gives the token stream structure. The one
exception is `RESERVED_WORDS`/`is_reserved_word`: not part of tokenizing
itself (every reserved word still lexes as an ordinary `Identifier` until
it earns its own `TokenKind`), but kept here rather than in `parser.rs`
so the one place that knows every word the grammar will ever treat
specially - lexed or not - is also the one place a future keyword's
`TokenKind` gets added.

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
  the original (case-sensitive) spelling. `VERBOSE` is deliberately absent
  from this keyword set - it always lexes as a plain `Identifier` here,
  and only `Parser::consume_verbose_keyword` (`parser.MD`) gives it
  special meaning, and only directly after `EXPLAIN`.
- `Lexer::lex_punct(&mut self, start) -> Result<TokenKind, SqlError>` -
  private. Scans one punctuation or operator token, including the
  two-character lookahead needed for `<=`, `<>`, `>=`, and `!=`.
- `Lexer::advance_while(&mut self, pred)` - private. Advances the cursor
  while `pred` holds on the current byte.
- `Lexer::char_at(&self, offset) -> char` - private. Decodes the UTF-8
  character starting at byte `offset`.
- `RESERVED_WORDS` - `pub(crate)`. The words that may never be treated as
  a bare table alias, even though most of them lex as an ordinary
  `Identifier` today - `ORDER`, `BY`, `GROUP`, `HAVING`, `LIMIT`,
  `OFFSET`, `JOIN`, `INNER`, `LEFT`, `RIGHT`, `FULL`, `OUTER`, `CROSS`,
  `ON`, `USING`, `UNION`, `INTERSECT`, `EXCEPT`, `RETURNING`, `SET`,
  `WHERE`, `FROM`. Seeded with every keyword a future grammar addition
  (`ORDER BY` in M20, `JOIN` in M11, `RETURNING` in M23, ...) will need,
  not only the ones tokenized so far, so `parser.MD`'s
  `parse_optional_table_alias` never has to be revisited just to add one
  more word - only `RESERVED_WORDS` does. A word appearing here before it
  has its own `TokenKind` is harmless: it still lexes as `Identifier`
  everywhere else in the grammar (`SELECT limit FROM t` still parses
  `limit` as a column reference), since this list only governs where an
  alias may appear, exactly the distinction Postgres's own reserved/
  unreserved keyword split maintains.
- `is_reserved_word(word) -> bool` - `pub(crate)`. Case-insensitive
  membership test against `RESERVED_WORDS`, matching how every keyword in
  `lex_ident_or_keyword` is already matched case-insensitively.

## Usage Example

```rust
use sql::Lexer;

let tokens = Lexer::new("SELECT * FROM t").tokenize()?;
```

`Parser::new` takes exactly the `Vec<Token>` `Lexer::tokenize` produces;
`engine::Database::execute` calls `Lexer::new(sql).tokenize()` as the
first step of running any SQL statement, rendering an `Err` against the
original source before it ever reaches the parser.
