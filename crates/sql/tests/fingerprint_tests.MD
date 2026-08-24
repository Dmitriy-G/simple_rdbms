# fingerprint_tests

Checks `fingerprint`'s literal-redaction against the shapes that actually
matter: the task's own worked example (an `INSERT` with a number and a
string), a float in a `WHERE` clause, exact preservation of everything
that isn't a literal (including original whitespace), a statement with
nothing to redact, a string literal containing characters that look like
SQL punctuation, and unlexable input.

## Key Components

- `integer_and_string_literals_are_redacted` - the task's own worked
  example: `INSERT INTO t VALUES (42, 'alice')` becomes
  `INSERT INTO t VALUES (?, ?)`.
- `a_float_literal_in_a_where_clause_is_redacted` - a `FloatLiteral` is
  redacted the same as an integer or string one.
- `identifiers_keywords_and_punctuation_are_preserved_exactly` - runs of
  irregular whitespace between non-literal tokens survive untouched, not
  just normalized to a single space.
- `a_statement_with_no_literals_is_unchanged` - fingerprinting is a no-op
  when there's nothing to redact.
- `a_string_literal_containing_a_comma_and_parens_is_redacted_as_one_token`
  - a string literal whose *contents* look like SQL structure doesn't
  confuse the redaction into leaving part of it visible or splitting it
  into more than one `?`.
- `unlexable_input_redacts_to_a_single_placeholder_rather_than_leaking_a_fragment`
  - an unterminated string literal (or any other lex failure) redacts to
  a bare `"?"` instead of falling back to showing the raw, unredacted text.

## Usage Example

```rust
use sql::fingerprint;

assert_eq!(fingerprint("SELECT * FROM t WHERE a = 1"), "SELECT * FROM t WHERE a = ?");
```

Run as part of `cargo test --workspace`; `engine`'s own tests separately
check that `Database::execute`'s `info`-level log line actually uses this
function's output rather than the raw SQL text.
