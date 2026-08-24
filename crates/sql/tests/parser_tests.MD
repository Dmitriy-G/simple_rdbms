# parser_tests

Parses each statement form and asserts on the resulting AST, checks
operator precedence, and checks that malformed input produces a located
parse error (not a panic).

## Key Components

- `try_parse(source) -> Result<Statement, SqlError>` - private. Runs the
  full `Lexer::tokenize` + `Parser::parse` pipeline over `source`.
- `parse(source) -> Statement` - private. `try_parse`, panicking with the
  source and error on failure; used by tests that expect success.
- `parse_err(source) -> SqlError` - private. `try_parse`, panicking with
  the source and resulting statement on unexpected success; used by tests
  that expect a specific parse error.
- `byte_offset(source, needle) -> usize` - private. The byte offset of
  `needle`'s first occurrence in `source`, panicking if absent; used to
  compute the expected offset in error-location assertions without
  hand-counting bytes.
- `parses_create_table` - a `CREATE TABLE` with `INTEGER`, `TEXT`, and
  `BOOLEAN` columns parses to the expected AST.
- `create_table_type_names_are_case_insensitive` - lowercase `create
  table` and `int` still parse correctly.
- `parses_insert_with_explicit_columns_and_multiple_rows` - an `INSERT`
  naming its columns with two value rows parses to the expected AST.
- `parses_insert_without_column_list` - an `INSERT` with no column list
  parses with an empty `columns` vec.
- `parses_select_wildcard` - `SELECT * FROM t` parses to
  `SelectItem::Wildcard`.
- `parses_select_list_and_where_clause` - a `SELECT` with an explicit
  column list and a `WHERE` clause parses to the expected AST.
- `precedence_or_binds_looser_than_and` - `a = 1 OR b = 2 AND c = 3`
  parses as `a=1 OR (b=2 AND c=3)`.
- `parentheses_override_precedence` - `(a = 1 OR b = 2) AND c = 3` parses
  with the parenthesized `OR` binding tighter than the outer `AND`.
- `unary_not_and_negate` - `NOT a AND b = -1` parses with `NOT` and unary
  `-` each binding to their immediate operand.
- `string_literal_escapes_doubled_quote` - `'it''s a test'` parses to the
  unescaped string `it's a test`.
- `keywords_are_case_insensitive_identifiers_are_not` - `select Id from
  MyTable` preserves the identifiers' original casing while matching
  `select`/`from` case-insensitively.
- `parses_begin_commit_rollback` - `BEGIN`, `COMMIT`, and `ROLLBACK` each
  parse to their `Statement` variant.
- `start_transaction_is_a_synonym_for_begin` - `START TRANSACTION` (in
  either case) parses to `Statement::Begin`.
- `transaction_control_keywords_are_case_insensitive` - lowercase
  `begin`/`commit`/`rollback` parse the same as uppercase.
- `start_without_transaction_is_a_parse_error_not_a_panic` - `START` with
  no following `TRANSACTION` errors with `UnexpectedEof` rather than
  panicking.
- `missing_closing_paren_errors_with_offset_not_panic` - an unterminated
  parenthesized expression errors with `UnexpectedEof`.
- `unterminated_string_errors_with_offset` - a string literal missing its
  closing quote errors with `UnterminatedString` pointing at the opening
  quote's offset.
- `trailing_comma_in_select_list_errors_not_panics` - a dangling comma in
  a `SELECT` list errors with `UnexpectedToken`.
- `oversized_integer_literal_is_a_lexer_error_quoting_the_text` - an
  integer literal too large for `i64` errors with `InvalidNumericLiteral`
  quoting the exact source text and offset.
- `float_literal_parses_as_a_double_value` - `1.5` parses to
  `Value::Double(1.5)`.
- `parses_bigint_and_double_column_types` - a `CREATE TABLE` with
  `BIGINT` and `DOUBLE` columns parses to the expected `DataType`s.
- `unexpected_char_reports_offset` - an illegal character (`@`) errors
  with `UnexpectedChar` naming the character and its offset.
- `render_points_a_caret_at_the_offending_token` - `SqlError::render`
  produces exactly three lines (message, source line, caret) with the
  caret aligned to the offending byte offset.

## Usage Example

```rust
#[test]
fn example_parses_a_select() {
    let tokens = sql::Lexer::new("SELECT * FROM t").tokenize().unwrap();
    let stmt = sql::Parser::new(tokens).parse().unwrap();
    assert!(matches!(stmt, sql::Statement::Select(_)));
}
```

Run as part of `cargo test --workspace`; this is the crate's grammar
coverage, exercising every statement form the parser accepts and every
`SqlError` variant it can raise, complementing `tests/smoke.rs`'s
bare-construction check.
