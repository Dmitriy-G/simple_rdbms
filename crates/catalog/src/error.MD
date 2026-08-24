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
- `impl From<CatalogError> for common::Error` - matches each variant onto
  the `common::Error` variant naming the same SQL condition:
  `TableAlreadyExists` -> `DuplicateTable`, `TableNotFound` ->
  `UndefinedTable`, `ColumnNotFound { table, column }` -> `UndefinedColumn`
  (with `name` formatted as `table.column`), `Storage` -> delegated to
  `storage::StorageError`'s own conversion, `Corrupt` -> `DataCorrupted`.

## Usage Example

```rust
use catalog::CatalogError;

match catalog.get_table("missing") {
    Err(CatalogError::TableNotFound(name)) => eprintln!("no such table: {name}"),
    _ => {}
}
```

Every fallible `Catalog` method returns `Result<_, CatalogError>`; callers
in `engine` propagate it with `?` and rely on the `From` impl to fold it,
structured field by field, into `common::Error` at the point it crosses
into `Database::execute`.
