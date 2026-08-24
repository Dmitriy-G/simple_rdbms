# catalog

The system catalog: an in-memory registry of every table's metadata, keyed
by name. Sits between the planner/executor and `storage`, resolving table
and column names to the physical locations operators read from and write
to.

Persisted as an ordinary `TableHeap` of its own, rooted at the database
header's `catalog_first_page`: every `create_table` call appends an
encoded `TableInfo` row to that heap, and `open` replays it to rebuild the
in-memory index. A `Catalog` built with `new` instead of `open` has no
catalog heap yet; `create_table` provisions one (and records its root page
in the header) the first time it's called with a live `BufferPool`.

## Key Components

- `Catalog` - the catalog struct: an in-memory index (`tables_by_name`,
  `tables_by_id`) plus the next `TableId` to assign and, once known, the
  root page of the catalog's own persisted `TableHeap`.
- `Catalog::new() -> Self` - creates an empty catalog with no storage
  backing yet.
- `Catalog::from_tables(tables) -> Self` - builds an in-memory catalog
  directly from already-constructed table entries, with no storage
  backing at all. Intended for tests that need a populated catalog (e.g.
  to exercise the planner's binder) without provisioning a `BufferPool`;
  `create_table` remains the way to register a table that needs real heap
  storage.
- `Catalog::open(buffer_pool, txn_id) -> Result<Self, CatalogError>` -
  opens the catalog backed by `buffer_pool`: finds (or provisions) its
  persisted table heap and loads every `TableInfo` row from it into
  memory. `txn_id` is the transaction the catalog's own bootstrap heap
  allocation (the first time a database is ever opened) is attributed to;
  the caller is responsible for committing it, so that this bootstrap
  write is a real, committed transaction rather than one crash recovery
  would undo as a loser.
- `Catalog::create_table(buffer_pool, txn_id, name, schema) -> Result<&TableInfo, CatalogError>` -
  registers a new table with the given `name` and `schema`, allocating its
  heap storage and a fresh `TableId`, and persists its catalog row.
- `Catalog::get_table(name) -> Result<&TableInfo, CatalogError>` - looks up
  a table's metadata by name.
- `Catalog::get_table_by_id(table_id) -> Result<&TableInfo, CatalogError>` -
  looks up a table's metadata by its stable id.
- `Catalog::table_names() -> Vec<&str>` - every registered table's name,
  sorted for a deterministic order.
- `Catalog::drop_table(name) -> Result<(), CatalogError>` - removes a
  table's metadata from the catalog. Does not reclaim its heap storage.
  Unimplemented (`todo!`).
- `Catalog::ensure_catalog_heap(buffer_pool, txn_id) -> Result<PageId, CatalogError>` -
  private. Returns the catalog's own table heap's root page, provisioning
  one (and recording it in the database header) if this is the first call
  against a catalog that didn't come from `open`.

## Usage Example

```rust
use catalog::Catalog;

let mut catalog = Catalog::open(&buffer_pool, bootstrap_txn)?;
catalog.create_table(&buffer_pool, txn_id, "users", schema)?;
let info = catalog.get_table("users")?;
```

`engine::Database` holds one `Catalog` for the lifetime of an open
database, built once via `open` at startup and rebuilt wholesale via
`open` again whenever a statement aborts, rather than trying to undo its
in-memory effects surgically. `tests/bootstrap_atomicity.rs` sweeps every
crash point across that very first `open` to check the catalog's bootstrap
heap allocation and the header pointer to it become durable together or
not at all.
