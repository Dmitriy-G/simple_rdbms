# smoke

Asserts the crate compiles and its main types can be constructed. This is
the minimum viable test for a foundational crate like `common`: it exists
to catch a broken build or a signature change that would otherwise only
surface indirectly, much later, in a downstream crate's own test failures.

## Key Components

- `db_config_constructs` - builds a `DbConfig` via `DbConfig::new` and
  checks its `page_size`/`buffer_pool_size` fall back to the documented
  defaults.
- `ids_construct_and_compare` - constructs two distinct `PageId`s and a
  `Rid`, checking equality/inequality and that `Rid::new` stores its
  fields as given.
- `error_displays` - constructs an `Error::NotSupported` and checks its
  `Display` output matches the format string on the variant.

## Usage Example

```rust
#[test]
fn example_uses_common_types_directly() {
    let config = common::DbConfig::new("test.db");
    assert_eq!(config.page_size, common::DbConfig::DEFAULT_PAGE_SIZE);
}
```

Run as part of `cargo test --workspace` alongside every other crate's own
tests; because `common` sits at the bottom of the dependency graph, a
failure here means every other crate's build is already suspect before
its own tests even run.
