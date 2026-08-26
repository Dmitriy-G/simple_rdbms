# lib

The system catalog: table, column, and index metadata. Tracks the schema
of every table and where its heap file begins, and which indexes exist on
which columns and where each one's B+tree root currently is, so the
planner and executor can resolve names to physical storage locations
without touching disk.

Persisted as two ordinary `TableHeap`s of its own: a table catalog rooted
at the database header's `catalog_first_page`, and an index catalog rooted
at `index_catalog_first_page`. Every `Catalog::create_table` call appends
an encoded `TableInfo` row to the first; every `Catalog::create_index`
call appends an encoded index row to the second; `Catalog::open` replays
both to rebuild the in-memory indexes. See `catalog.MD` for how that
bootstrap sequence is made crash-safe, and for why an index's current root
page is deliberately never cached in memory.

## Key Components

- `Catalog` - the in-memory registry of every table's and every index's
  metadata, keyed by name. See `catalog.MD`.
- `Column` - a single column's name, declared type, and nullability. See
  `column.MD`.
- `CatalogError` - errors raised by the catalog. See `error.MD`.
- `IndexInfo` - an index's catalog identity: which index, which table and
  column. See `index_info.MD`.
- `Schema` - an ordered list of columns describing a table's tuple shape.
  See `schema.MD`.
- `TableInfo` - a table's catalog identity, schema, and heap root page.
  See `table_info.MD`.

## Usage Example

```rust
use catalog::{Catalog, Column, Schema};
use types::DataType;

let mut catalog = Catalog::open(&buffer_pool, bootstrap_txn)?;
let schema = Schema::new(vec![Column::new("id", DataType::Integer, false)]);
let info = catalog.create_table(&buffer_pool, txn_id, "users", schema)?.clone();
catalog.create_index(&buffer_pool, txn_id, "idx_users_id", info.table_id, 0)?;
```

`engine::Database::open_with_managers` opens the catalog once, at startup,
against the live `BufferPool`; `Database::execute` then calls
`create_table`/`create_index` and `get_table`/`index_for_column` as DDL and
query statements flow through, and reloads the catalog wholesale (via
`Catalog::open` again) on an explicit `ROLLBACK`, rather than trying to
undo its in-memory effects surgically.
