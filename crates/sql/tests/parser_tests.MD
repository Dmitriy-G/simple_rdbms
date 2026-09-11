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
- `col_expr(name) -> Expr` - private. `Expr::Column { table: None, name }`,
  shorthand for the bare-column case every pre-B-2 test still exercises.
- `table_ref(name) -> Option<TableRef>` - private.
  `Some(TableRef { name, alias: None })`, shorthand for an unaliased
  `FROM` target in the `Option` shape `SelectStatement::from` now has.
- `parses_create_table` - a `CREATE TABLE` with `INTEGER`, `TEXT`, and
  `BOOLEAN` columns parses to the expected AST.
- `create_table_type_names_are_case_insensitive` - lowercase `create
  table` and `int` still parse correctly.
- `parses_create_index` - `CREATE INDEX idx_users_id ON users (id)` parses
  to the expected `CreateIndexStatement`.
- `create_index_and_create_table_are_disambiguated_by_one_token_of_lookahead` -
  `create index idx on t (a)` and `create table t (a int)` each parse to
  their own statement kind, proving `Parser::peek_kind`'s one-token
  lookahead after `CREATE` picks correctly.
- `create_index_missing_on_is_a_parse_error`,
  `create_index_missing_parens_around_column_is_a_parse_error` - malformed
  `CREATE INDEX` grammar (missing `ON`, missing the column's parentheses)
  each report `SqlError::UnexpectedToken` rather than panicking.
- `parses_insert_with_explicit_columns_and_multiple_rows` - an `INSERT`
  naming its columns with two value rows parses to the expected AST.
- `parses_insert_without_column_list` - an `INSERT` with no column list
  parses with an empty `columns` vec.
- `parses_select_wildcard` - `SELECT * FROM t` parses to
  `SelectItem::Wildcard`.
- `parses_select_list_and_where_clause` - a `SELECT` with an explicit
  column list and a `WHERE` clause parses to the expected AST.
- `parses_qualified_column_reference` - `t.a` in the select list and `t.b`
  in the `WHERE` clause both parse to `Expr::Column { table: Some("t"),
  .. }`.
- `parses_a_select_with_no_from_clause` - `SELECT 1` and `SELECT 'keep
  alive'` parse with `from: None` instead of failing on a missing `FROM`.
- `parses_several_expressions_with_no_from_clause` - `SELECT 1, 'x'`
  keeps both items in order with no `FROM` clause.
- `a_wildcard_with_no_from_clause_is_a_syntax_error` - `SELECT *` alone
  reports `SqlError::WildcardWithoutFrom`.
- `parses_table_alias_with_and_without_as` - `FROM t AS u` and `FROM t u`
  both parse to the same `TableRef { name: "t", alias: Some("u") }`.
- `from_without_an_alias_still_allows_a_where_clause` - `FROM t WHERE a =
  1` parses `from` with `alias: None` and still finds the `WHERE`
  clause - proving `parse_optional_table_alias` correctly declines to
  consume `WHERE` as a bare alias.
- `order_by_is_not_swallowed_as_a_table_alias` - `SELECT * FROM t ORDER BY
  x` reports `SqlError::UnexpectedToken` naming `ORDER`, proving
  `parse_optional_table_alias` declines a reserved word rather than
  consuming it as `t`'s alias and failing later on `BY` instead.
- `table_alias_declines_reserved_words_not_yet_tokenized` - the same
  check for a representative sweep of `lexer::RESERVED_WORDS` words that
  have no `TokenKind` of their own yet (`ORDER`, `GROUP`, `HAVING`,
  `LIMIT`, `JOIN`, `RETURNING`, `UNION`, `OFFSET`): each is left
  unconsumed rather than swallowed as an alias.
- `table_alias_declines_reserved_words_case_insensitively` - lowercase
  `order` is declined exactly like `ORDER`, matching
  `is_reserved_word`'s case-insensitive comparison.
- `precedence_or_binds_looser_than_and` - `a = 1 OR b = 2 AND c = 3`
  parses as `a=1 OR (b=2 AND c=3)`.
- `parentheses_override_precedence` - `(a = 1 OR b = 2) AND c = 3` parses
  with the parenthesized `OR` binding tighter than the outer `AND`.
- `unary_not_and_negate` - `NOT a AND b = -1` parses with `NOT` and unary
  `-` each binding to their immediate operand.
- `parses_is_null_and_is_not_null` - `a IS NULL` and `a IS NOT NULL` each
  parse to `Expr::IsNull` with the expected `negated` flag.
- `is_null_binds_tighter_than_and_but_looser_than_comparison` - `a = 1 AND
  b IS NULL` parses with `IS NULL` applying to `b` alone, then combined
  with the `a = 1` comparison by `AND`.
- `is_null_binds_looser_than_comparison` - `a = b IS NULL` parses as
  `(a = b) IS NULL`, not `a = (b IS NULL)` - matching Postgres, where `IS`
  sits below the comparison operators.
- `is_null_missing_null_keyword_is_a_parse_error` - `a IS` with nothing
  after it errors with `UnexpectedEof` rather than panicking.
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
- `parses_explain_select` - `EXPLAIN SELECT * FROM t` parses to
  `Statement::Explain { verbose: false, inner: Box::new(Statement::Select(...)) }`.
- `parses_explain_verbose`, `explain_verbose_is_case_insensitive` -
  `EXPLAIN VERBOSE <select>` sets `verbose: true`, in either letter case.
- `explain_of_insert_and_create_statements_parses` - `EXPLAIN` accepts
  `INSERT`, `CREATE TABLE`, and `CREATE INDEX` as its target, not only
  `SELECT`.
- `select_verbose_still_parses_verbose_as_a_column_reference` - `SELECT
  verbose FROM t` parses `verbose` as an ordinary `Expr::Column`, proving
  `VERBOSE` never became a reserved word outside the one grammar position
  right after `EXPLAIN`.
- `explain_of_begin_commit_rollback_is_a_parse_error` - `EXPLAIN BEGIN`/
  `EXPLAIN COMMIT`/`EXPLAIN ROLLBACK` each report
  `SqlError::ExplainOfTransactionControl` rather than parsing.
- `nested_explain_is_a_parse_error` - `EXPLAIN EXPLAIN SELECT * FROM t`
  reports `SqlError::NestedExplain`.
- `parses_dollar_placeholder_in_where_clause` - `WHERE a = $1` parses `$1`
  to `Expr::Parameter { index: 1 }` in primary-expression position.
- `parses_dollar_placeholders_in_insert_values` - `INSERT INTO t VALUES
  ($1, $2)` parses both placeholders to `Expr::Parameter`, in order.
- `dollar_zero_is_a_lexer_error` - `$0` reports
  `SqlError::InvalidParameter` quoting `"$0"` at the `$`'s offset, since
  placeholder indices are 1-based.
- `bare_dollar_with_no_digit_is_a_lexer_error` - a `$` with no following
  digit reports `SqlError::InvalidParameter` quoting `"$"`.
- `oversized_parameter_index_is_a_lexer_error` - a digit run too large for
  `u32` after `$` reports `SqlError::InvalidParameter` quoting the whole
  span.
- `session_context_items(source) -> Vec<SessionContextName>` - private.
  Parses a `SELECT` and asserts every select item is a session-context
  expression, returning the names, so the tests below read as a list of
  spellings rather than as nested pattern matches.
- `every_session_context_spelling_parses_with_and_without_parentheses` -
  `current_catalog`/`current_database()` both yield `CurrentCatalog`,
  `current_schema`/`current_schema()` both yield `CurrentSchema`,
  `current_user`/`user` both yield `CurrentUser` while `session_user`
  stays its own variant, and `version()` yields `Version`.
- `a_session_context_name_is_recognized_whatever_its_case` -
  `CURRENT_SCHEMA` and `Current_Catalog()`, since the lexer preserves an
  identifier's case and the match is deliberately case-insensitive.
- `a_session_context_name_inside_a_where_clause_is_an_ordinary_expression` -
  `WHERE a = current_schema` puts a `SessionContext` on the right of a
  `BinaryOp`, which is the half of P-91 a text-matching interception in
  the wire layer could never have answered.
- `a_qualified_name_is_still_a_column_even_when_it_ends_in_a_session_context_word` -
  `t.version` stays `Expr::Column`; the qualifier decides it before the
  allowlist is consulted at all.
- `a_function_spelling_without_parentheses_is_an_ordinary_column` -
  `SELECT version, current_database FROM t` binds two column references.
  This is the test that stops the allowlist stealing a user's column
  name: only the parenthesized form is the session-context value.
- `a_bare_unknown_name_is_still_a_column_reference` - `SELECT now FROM t`
  is a column, not an error; the allowlist only applies in call position.
- `an_unknown_function_call_is_an_undefined_function_not_a_syntax_error` -
  `now()` reports `SqlError::UndefinedFunction` at the identifier's own
  offset, which `engine` maps to `42883`.
- `a_session_context_name_with_arguments_is_an_undefined_function` -
  `current_schema(1)` is refused rather than having its argument ignored.
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
