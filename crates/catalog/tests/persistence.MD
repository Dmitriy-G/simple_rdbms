# persistence

Catalog persistence: tables created against one `BufferPool` must
reappear, with identical schemas, when the database file is reopened;
duplicate table names are rejected.

## Key Components

- `open_pool(path) -> Result<BufferPool, Box<dyn Error>>` - private.
  Builds a real, file-backed `BufferPool` (disk, WAL, and double-write
  buffer all rooted at `path`).
- `users_schema() -> Schema` - private. A small two-column schema shared
  by several tests.
- `reopening_the_database_reloads_identical_schemas` - creates two tables
  against one `BufferPool`, flushes, reopens a fresh `BufferPool` and
  `Catalog` against the same file, and checks both tables' schemas and
  the by-id lookup survive the round trip.
- `schemas_with_more_than_255_columns_round_trip` - creates a table with
  256 columns to check the schema blob's length-prefixed encoding doesn't
  silently truncate at a single-byte boundary, then reopens and checks the
  column count and schema still match.
- `duplicate_table_name_is_rejected` - creates a table, then creates it
  again with the same name, and checks the second call returns
  `CatalogError::TableAlreadyExists` rather than overwriting the first.

## Usage Example

```rust
#[test]
fn example_round_trips_a_table_across_reopen() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("test.db");
    {
        let pool = open_pool(&path)?;
        let mut catalog = catalog::Catalog::open(&pool, TXN)?;
        catalog.create_table(&pool, TXN, "users", users_schema())?;
        pool.flush_all()?;
    }
    let pool = open_pool(&path)?;
    let catalog = catalog::Catalog::open(&pool, TXN)?;
    assert_eq!(catalog.table_names(), vec!["users"]);
    Ok(())
}
```

Run as part of `cargo test --workspace`; exercises the full round trip
through `persist::encode_table_info`/`decode_table_info` that
`tests/bootstrap_atomicity.rs` assumes already works correctly and instead
focuses on crash timing.
