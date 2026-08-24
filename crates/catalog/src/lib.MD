# lib

The system catalog: table and column metadata. Tracks the schema of every
table and where its heap file begins, so the planner and executor can
resolve names to physical storage locations without touching disk.

Persisted as an ordinary `TableHeap` of its own, rooted at the database
header's `catalog_first_page`: every `Catalog::create_table` call appends
an encoded `TableInfo` row to that heap, and `Catalog::open` replays it to
rebuild the in-memory index. See `catalog.MD` for how that bootstrap
sequence is made crash-safe.

## Key Components

- `Catalog` - the in-memory registry of every table's metadata, keyed by
  name. See `catalog.MD`.
- `Column` - a single column's name, declared type, and nullability. See
  `column.MD`.
- `CatalogError` - errors raised by the catalog. See `error.MD`.
- `Schema` - an ordered list of columns describing a table's tuple shape.
  See `schema.MD`.
- `TableInfo` - a table's catalog identity, schema, and heap root page.
  See `table_info.MD`.

## Usage Example

```rust
use catalog::{Catalog, Column, Schema};
use types::DataType;

let catalog = Catalog::open(&buffer_pool, bootstrap_txn)?;
let schema = Schema::new(vec![Column::new("id", DataType::Integer, false)]);
catalog.create_table(&buffer_pool, txn_id, "users", schema)?;
```

`engine::Database::open_with_managers` opens the catalog once, at startup,
against the live `BufferPool`; `Database::execute` then calls
`create_table` and `get_table` as `CREATE TABLE` and query statements flow
through, and reloads the catalog wholesale (via `Catalog::open` again)
after any statement that aborts, rather than trying to undo its in-memory
effects surgically.
