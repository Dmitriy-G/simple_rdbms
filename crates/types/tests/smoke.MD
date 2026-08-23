# smoke

Asserts the crate compiles and its main types can be constructed. Same
role as `common`'s own `tests/smoke.MD`: the minimum viable test for a
low-level crate, catching a broken build or a signature change before it
only surfaces indirectly in a downstream crate's own test failures.

## Key Components

- `tuple_constructs_from_values` - builds a `Tuple` from a mix of typed
  and `Null` values and checks its length and that the `Null` slot reports
  itself as null via `is_null`.
- `data_type_variants_construct` - constructs one value of every
  `DataType` variant, including a `Varchar` with an explicit length, and
  checks the count.

## Usage Example

```rust
#[test]
fn example_uses_types_directly() {
    let tuple = types::Tuple::new(vec![types::Value::Integer(1)]);
    assert_eq!(tuple.values().len(), 1);
}
```

Run as part of `cargo test --workspace` alongside every other crate's own
tests; because `types` sits just above `common` in the dependency graph, a
failure here means every crate above it (`catalog`, `sql`, `planner`,
`executor`, `engine`, `cli`) is already suspect before its own tests even
run.
