# error

Errors raised by the system catalog.

## Key Components

- `CatalogError` - the enum. `TableAlreadyExists(String)` is raised when a
  `CREATE TABLE` names a table that already exists. `TableNotFound(String)`
  is raised when a statement references a table the catalog has no entry
  for. `ColumnNotFound { table, column }` is raised when a statement
  references a column the table's schema has no entry for. `Storage`
  wraps a `storage::StorageError` from the catalog's own persisted table
  heap. `Corrupt(String)` is raised when a persisted catalog row can't be
  decoded back into a `TableInfo` (truncated row, invalid schema blob,
  unknown type tag).
- `impl From<CatalogError> for common::Error` - converts into the
  workspace-wide error type as `Error::Catalog`, carrying the original
  error's `Display` text.

## Usage Example

```rust
use catalog::CatalogError;

match catalog.get_table("missing") {
    Err(CatalogError::TableNotFound(name)) => eprintln!("no such table: {name}"),
    _ => {}
}
```

Every fallible `Catalog` method returns `Result<_, CatalogError>`; callers
in `engine` propagate it with `?` and rely on the `From` impl to fold it
into `common::Error` at the point it crosses into `Database::execute`.
