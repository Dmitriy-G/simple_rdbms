# smoke

Asserts the crate compiles and its main types can be constructed. Same
minimum-viable role as `common`'s and `types`' own `smoke.rs` files.

## Key Components

- `page_constructs_zeroed` - `Page::new` yields the given id and all-zero
  bytes.
- `lru_k_replacer_constructs` - `LruKReplacer::new` doesn't panic.
- `log_record_constructs` - a `LogRecord` can be built and its `kind`
  matched.
- `storage_error_displays` - `StorageError::PageNotFound`'s `Display`
  output matches its format string.

## Usage Example

```rust
#[test]
fn example_uses_storage_types_directly() {
    let page = storage::page::Page::new(common::PageId(0));
    assert!(page.data().iter().all(|&b| b == 0));
}
```

Run as part of `cargo test --workspace`; because `storage` sits below
`catalog`/`txn`/`executor`/`engine`, a failure here means every crate
above it is already suspect before its own tests even run.
