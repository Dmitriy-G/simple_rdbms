# table_info

Everything the rest of the engine needs to know about one table: its
catalog identity, its schema, and where its heap storage begins.

## Key Components

- `TableInfo` - the table struct. `table_id` is the table's stable
  identity within the catalog. `name` is unique within the catalog.
  `schema` is the table's column schema. `first_page_id` is the first page
  of the table's heap file, the entry point for a sequential scan.
- `TableInfo::new(table_id, name, schema, first_page_id) -> Self` - builds
  a table's catalog entry.

## Usage Example

```rust
use catalog::TableInfo;

let info = TableInfo::new(table_id, "users", schema, first_page_id);
```

`Catalog::create_table` builds a `TableInfo` from a fresh `TableId` and a
newly allocated `TableHeap`'s first page, and stores it in the catalog's
in-memory index; `Catalog::open` rebuilds the same `TableInfo`s by decoding
them back out of the catalog's persisted heap.
