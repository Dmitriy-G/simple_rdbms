# smoke

Asserts the crate compiles and its main types can be constructed.

## Key Components

- `catalog_constructs_empty` - builds a `Catalog` via `Catalog::new` and
  checks a lookup on it returns an error rather than panicking.
- `schema_constructs_from_columns` - builds a `Schema` from a `Vec<Column>`
  and checks its `columns()` length.

## Usage Example

```rust
#[test]
fn example_constructs_empty_catalog() {
    let catalog = catalog::Catalog::new();
    assert!(catalog.get_table("missing").is_err());
}
```

Run as part of `cargo test --workspace`; catches a broken build or a
signature change in `Catalog`/`Schema` before it surfaces indirectly in a
downstream crate's tests.
