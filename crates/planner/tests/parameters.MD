# parameters

Exercises `planner::infer_parameter_types` and
`planner::substitute_parameters` against a populated catalog: type
inference from a `WHERE` comparison and from an `INSERT`'s named column
list, the unknown-type and numbering-gap cases, and that a substituted
statement binds identically to the equivalent literal one.

## Key Components

- `catalog_with_users() -> Catalog` - private. A catalog with one table,
  `users(id INTEGER, name VARCHAR(64), active BOOLEAN)`.
- `catalog_with_ab() -> Catalog` - private. A catalog with one table,
  `t(a INTEGER, b VARCHAR(32))`, used to prove an `INSERT`'s explicit
  column list - not table order - drives which type a placeholder infers.
- `parse(source) -> sql::Statement` - private. Runs `sql`'s lexer and
  parser over `source`, panicking on a parse error.
- `infer(catalog, source) -> Result<Vec<Option<DataType>>, PlannerError>` -
  private. Parses `source` and calls `infer_parameter_types` against
  `catalog`.
- `bind_debug(catalog, source) -> String` - private. Parses, binds, and
  `Debug`-formats `source`'s bound statement; used because
  `BoundStatement`/`BoundExpr` derive `Debug` but not `PartialEq`, so
  comparing two bound plans for this test means comparing their debug
  strings.
- `infers_parameter_type_from_a_where_clause_comparison` - `WHERE id = $1`
  against an `Integer` column infers `[Some(Integer)]`.
- `insert_parameter_types_follow_the_named_column_list_not_table_order` -
  `INSERT INTO t (b, a) VALUES ($1, $2)` infers `[Some(Varchar(32)),
  Some(Integer)]` - `$1` takes `b`'s type and `$2` takes `a`'s, following
  the statement's column list rather than the table's declared order.
- `a_placeholder_with_no_column_context_is_unknown` - a bare `$1` in a
  select list with nothing to compare it against infers `None`.
- `a_numbering_gap_is_undefined_parameter` - `$1` and `$3` with no `$2`
  used anywhere in the statement returns `PlannerError::UndefinedParameter { index: 2 }`.
- `substitution_produces_the_same_bound_plan_as_the_equivalent_literal_statement` -
  `substitute_parameters` on `WHERE id = $1` with `[Value::Integer(1)]`,
  then bound, produces the same `Debug` output as binding `WHERE id = 1`
  directly - proving substitution and literal coercion agree.
- `too_few_values_is_a_parameter_count_mismatch`,
  `too_many_values_is_a_parameter_count_mismatch` - supplying zero or two
  values for a statement referencing exactly `$1` both return
  `PlannerError::ParameterCountMismatch { expected: 1, found }`.

## Usage Example

```rust
#[test]
fn example_infers_a_parameter_type() {
    let catalog = catalog_with_users();
    let types = infer(&catalog, "SELECT * FROM users WHERE id = $1").unwrap();
    assert_eq!(types, vec![Some(types::DataType::Integer)]);
}
```

Run as part of `cargo test --workspace`; this is the crate's coverage for
`parameters.rs`, complementing `tests/binder_tests.rs`'s coverage of the
binder's own `UndefinedParameter` rejection.
