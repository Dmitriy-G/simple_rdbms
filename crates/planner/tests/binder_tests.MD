# binder_tests

Binds parsed statements against a populated catalog and checks that
unknown columns/tables and type mismatches are rejected.

## Key Components

- `catalog_with_users() -> Catalog` - private. A catalog with one table,
  `users(id INTEGER, name VARCHAR(64), active BOOLEAN)`, built via
  `Catalog::from_tables` (no storage backing).
- `catalog_with_numeric_columns() -> Catalog` - private. A catalog with
  one table, `nums(id INTEGER, big BIGINT)`, used to test cross-numeric-
  type comparisons.
- `parse(source) -> sql::Statement` - private. Runs `sql`'s lexer and
  parser over `source`, panicking on a parse error (every source string
  in this file is expected to parse; binding is what's under test).
- `bind(catalog, source) -> Result<BoundStatement, PlannerError>` -
  private. Parses `source` and binds it against `catalog`.
- `bind_ok(catalog, source) -> BoundStatement` - private. `bind`,
  panicking with the source and error on failure; used by tests that
  expect a successful bind.
- `binds_select_wildcard_to_every_column` - `SELECT * FROM users` expands
  to one projection per column and no predicate.
- `binds_select_with_matching_where_clause` - a `WHERE` clause comparing
  an `Integer` column and an `AND`-ed `Boolean` column binds successfully.
- `rejects_unknown_table` - selecting from a table absent from the catalog
  returns `PlannerError::UnknownTable` naming it.
- `rejects_unknown_column_in_select_list` - a select-list column absent
  from the table's schema returns `PlannerError::UnknownColumn` naming it.
- `rejects_unknown_column_in_where_clause` - same, for a column referenced
  only in `WHERE`.
- `rejects_comparison_between_mismatched_types` - comparing an `Integer`
  column against a string literal returns `PlannerError::TypeMismatch`.
- `rejects_non_boolean_and_operand` - using a non-`Boolean` column
  directly as an `AND` operand returns `PlannerError::TypeMismatch`.
- `rejects_insert_value_type_mismatch` - inserting a string literal into
  an `Integer` column returns `PlannerError::TypeMismatch`.
- `rejects_insert_column_count_mismatch` - supplying fewer values than
  named columns returns `PlannerError::ColumnCountMismatch` with the
  expected and found counts.
- `insert_without_column_list_defaults_missing_trailing_columns_are_not_allowed` -
  omitting the column list still requires a value for every column in
  schema order; two values against a three-column table returns
  `ColumnCountMismatch { expected: 3, found: 2 }`.
- `insert_leaves_unlisted_columns_null` - inserting only `id` into the
  three-column `users` table produces one row with 3 bound values, the
  other two implicitly `Value::Null`.
- `integer_literal_narrows_to_the_integer_column_it_is_compared_against` -
  `WHERE id = 1` binds the `BigInt` literal `1` down to
  `Value::Integer(1)` to match the `Integer` column it's compared against.
- `integer_literal_narrows_to_the_integer_column_it_is_inserted_into` -
  the same narrowing applies to an `INSERT` value bound against an
  `Integer` column.
- `oversized_literal_into_integer_column_is_out_of_range_not_type_mismatch` -
  a literal one past `i32::MAX` inserted into an `Integer` column returns
  `PlannerError::LiteralOutOfRange` (not `TypeMismatch`), naming the
  column.
- `oversized_literal_against_integer_column_in_where_clause_is_out_of_range` -
  the same oversized-literal check applies to a `WHERE` comparison, not
  just `INSERT`.
- `rejects_comparison_between_integer_and_bigint_columns` - comparing an
  `Integer` column against a `BigInt` column (no literal on either side to
  coerce) returns `PlannerError::TypeMismatch`.
- `binds_is_null_and_is_not_null_to_boolean` - `WHERE id IS NULL` and
  `WHERE id IS NOT NULL` each bind to a `BoundExpr::IsNull` predicate with
  the expected `negated` flag.
- `is_null_accepts_any_operand_type_without_coercion` - `IS NULL`/`IS NOT
  NULL` on a `Varchar` column and a `Boolean` column both bind
  successfully, since nullness testing places no type restriction on its
  operand.
- `binds_create_table` - `CREATE TABLE t (a INTEGER, b TEXT)` binds to a
  `BoundCreateTable` with the given name and two columns.

## Usage Example

```rust
#[test]
fn example_rejects_unknown_table() {
    let catalog = catalog_with_users();
    let err = bind(&catalog, "SELECT * FROM missing").unwrap_err();
    assert!(matches!(err, planner::PlannerError::UnknownTable(_)));
}
```

Run as part of `cargo test --workspace`; this is the crate's binder
coverage, exercising every `PlannerError` variant the binder can raise and
the literal-coercion behavior in `binder.rs`'s `coerce_literal_to_column`,
complementing `tests/smoke.rs`'s bare-construction check.
