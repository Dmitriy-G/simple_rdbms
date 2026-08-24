# error

Errors raised while lexing or parsing SQL text.

## Key Components

- `SqlError` - the enum. `UnexpectedChar { ch, offset }` is raised when
  the lexer finds a byte that cannot start any valid token. `ch` is the
  offending character and `offset` its byte offset in the source text.
  `UnterminatedString { offset }` is raised when a string or quoted
  identifier is never closed before end of input; `offset` is the byte
  offset of the opening quote. `UnexpectedToken { expected, found, offset }`
  is raised when the parser finds a token where a different construct was
  expected; `expected` describes what the grammar expected at this point,
  `found` is the token kind actually found (formatted for display), and
  `offset` is the offending token's byte offset. `UnexpectedEof { expected }`
  is raised when the token stream ends while the parser still expected
  more input; `expected` describes what the grammar expected next.
  `InvalidNumericLiteral { text, offset }` is raised when a numeric
  literal's text could not be parsed as the numeric type its shape implies
  (an integer literal too large for `i64`, or a float literal outside
  `f64`'s range), rather than being silently replaced with a sentinel
  value; `text` is the exact source text the user typed and `offset` is
  where the literal starts.
- `SqlError::offset(source) -> usize` - the byte offset in `source` this
  error points at. An `UnexpectedEof` has no token of its own to point at,
  so it points one past the end of `source`.
- `SqlError::render(source) -> String` - renders this error against the
  `source` it came from as a message followed by the offending line and a
  caret pointing at the exact byte offset.
- `impl From<SqlError> for common::Error` - converts into the
  workspace-wide error type as `Error::Parse`, carrying the original
  error's `Display` text.

## Usage Example

```rust
use sql::{Lexer, Parser};

let source = "SELECT * FROM t WHERE a = @";
let tokens = Lexer::new(source).tokenize();
if let Err(err) = tokens {
    eprintln!("{}", err.render(source));
}
```

`engine::Database::execute` calls `SqlError::render` against the original
SQL text before converting the error into `common::Error::Parse`, so a
malformed statement's error message always shows the user exactly where
in their query it went wrong; `tests/parser_tests.rs` checks
`render`'s caret placement directly.
