# integration

End-to-end tests of the full lex -> parse -> bind -> plan -> execute
pipeline, against a real (tempfile-backed) database file.

## Key Components

- `open(dir) -> Database` - opens a `Database` at `dir.join("test.db")`.
- `rows_of(result) -> (Vec<String>, Vec<Vec<Value>>)` - unwraps a
  `ResultSet::Rows` into its columns and each row's values, panicking on
  `RowsAffected`.
- `create_insert_and_select_star_preserves_insertion_order` - `CREATE
  TABLE`, a multi-row `INSERT`, and `SELECT *` come back in insertion
  order.
- `select_list_can_subset_and_reorder_columns` - `SELECT b, a` returns
  exactly those two columns in that order.
- `where_clause_evaluates_eq_lt_and_or_and_excludes_null_predicate_rows` -
  a `WHERE` combining `=`, `<`, `OR`, and `AND` excludes a row whose
  predicate involves `NULL` and therefore evaluates to `NULL` (not
  `false`) - the row is still excluded either way, since `NULL` is not
  `true`.
- `insert_spanning_multiple_pages_is_all_readable_back` - 1000 single-
  column rows (enough to span several heap pages) all round-trip through
  `INSERT` then `SELECT`.
- `data_survives_close_and_reopen` - data inserted before a clean `close`
  is still present after reopening the same file.
- `repeated_close_and_reopen_cycles_accumulate_tables_without_extra_page_growth`
  - ten open/`CREATE TABLE`/close cycles produce a file exactly `2 +
  CYCLES` pages long: page 0 is the header, one more page is the
  catalog's own heap (provisioned once, by the first `CREATE TABLE` - see
  `Catalog::ensure_catalog_heap`), and each of the `CYCLES` tables gets
  exactly one heap page of its own. Any extra growth here would mean a
  reopen is re-deriving `next_page_id` wrong and allocating pages it
  doesn't need.
- `integer_bigint_and_double_columns_round_trip_their_own_value_variants`
  - `INTEGER`, `BIGINT`, and `DOUBLE` columns each come back as their own
  `Value` variant, not silently coerced to another numeric type.
- `where_clause_integer_literal_narrows_against_an_integer_column` - an
  integer literal in a `WHERE` clause compares correctly against an
  `INTEGER` column.
- `oversized_literal_into_integer_column_is_an_out_of_range_error_not_a_type_mismatch`
  - inserting `i32::MAX + 1` into an `INTEGER` column is reported as an
  out-of-range error naming the column, not a type mismatch.
- `comparing_integer_column_to_bigint_column_is_still_an_error` -
  `WHERE a = b` across an `INTEGER` and a `BIGINT` column is rejected;
  the two types are not implicitly compared.
- `oversized_integer_literal_is_a_lexer_error_quoting_the_text` - a
  literal too large for any integer type is a lexer error that quotes the
  offending text.
- `select_from_nonexistent_table_is_an_error` - `SELECT * FROM missing`
  errors rather than panicking or returning an empty result.
- `a_parse_error_carries_sqlstate_42601_and_its_byte_offset` - a lexer
  error surfaced through `Database::execute` comes back as
  `common::Error::Syntax { offset, .. }` with `sql_state() ==
  SqlState::SYNTAX_ERROR`, and `offset` is exactly the byte position of
  the offending character in the original source text - not just baked
  into the rendered message.

## Usage Example

```rust
let dir = tempfile::tempdir().expect("create temp dir");
let mut db = open(&dir);
db.execute("CREATE TABLE t (a INTEGER, b INTEGER)").expect("create table");
db.execute("INSERT INTO t VALUES (1, 10), (2, 20)").expect("insert");
let (columns, rows) = rows_of(db.execute("SELECT * FROM t").expect("select"));
```

Run as part of `cargo test --workspace`; each test drives `Database`
through its public `execute` API only, the same way `cli` does, so these
exercise the whole pipeline rather than any one crate in isolation.
