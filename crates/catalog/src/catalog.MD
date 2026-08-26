# catalog

The system catalog: an in-memory registry of every table's and every
index's metadata, keyed by name. Sits between the planner/executor and
`storage`, resolving table, column, and index names to the physical
locations operators read from and write to.

Persisted as two ordinary `TableHeap`s of its own: a table catalog heap
rooted at the database header's `catalog_first_page`, and an index catalog
heap rooted at `index_catalog_first_page` (`storage::disk.MD`), each
following the same pattern. Every `create_table` call appends an encoded
`TableInfo` row to the table catalog heap; every `create_index` call
appends an encoded `(IndexInfo, root_page_id)` row to the index catalog
heap; `open` replays both to rebuild the in-memory indexes. A `Catalog`
built with `new` instead of `open` has neither heap yet; `create_table`/
`create_index` each provision their own heap (and record its root page in
the header) the first time they're called with a live `BufferPool`.

**No in-memory root-page cache.** `IndexInfo` (`index_info.MD`) does not
carry `root_page_id` - only `index_id`/`name`/`table_id`/`column_index`,
which never change after `create_index`. `storage::btree::BTreeIndex::insert`
mutates its own `root_page_id` on a root split, and the only caller that
can observe that (`executor::InsertExecutor`, maintaining an index as it
inserts rows) only ever holds a shared `&Catalog` - there is no mutable
path to update a cached field from there. Caching it anyway (e.g. behind a
`Cell`) would go stale across an aborted statement:
`engine::Database::execute_non_control_statement`'s autocommit-error path
undoes the failed statement's WAL-logged writes (including any index
catalog row rewrite) but, unlike an explicit `ROLLBACK`, never reloads
`self.catalog` afterward - so a cached root page would silently diverge
from the just-undone on-disk value. Instead, `index_root_page` always
re-scans the small index catalog heap for the current value, and
`update_index_root_page` does the same scan to find and rewrite the row.
Neither method needs `&mut self`, matching
`storage::buffer::BufferPool::set_catalog_first_page`'s existing `&self`
pattern - nothing on the Rust `Catalog` struct itself changes, only
durable heap state.

## Key Components

- `Catalog` - the catalog struct: an in-memory table index
  (`tables_by_name`, `tables_by_id`) plus the next `TableId` to assign and
  the table catalog heap's root page, and an in-memory index registry
  (`indexes_by_name`, `indexes_by_id`, `indexes_by_table` - the last
  mapping a `TableId` to every `IndexId` covering it, for
  `indexes_for_table`) plus the next `IndexId` to assign and the index
  catalog heap's root page. Both root pages are `Option<PageId>`, `None`
  until the corresponding heap is first provisioned.
- `Catalog::new() -> Self` - creates an empty catalog with no storage
  backing yet.
- `Catalog::from_tables(tables) -> Self` - builds an in-memory catalog
  directly from already-constructed table entries, with no storage
  backing at all. Intended for tests that need a populated catalog (e.g.
  to exercise the planner's binder) without provisioning a `BufferPool`;
  `create_table` remains the way to register a table that needs real heap
  storage.
- `Catalog::from_tables_and_indexes(tables, indexes) -> Self` -
  `from_tables`, plus registering already-constructed `IndexInfo` entries
  the same in-memory-only way. Intended for tests that need
  `index_for_column`/`indexes_for_table` populated (e.g. to exercise
  `planner::optimizer::IndexScanRule`) without a `BufferPool` or a real
  `storage::btree::BTreeIndex` - `index_root_page`/`update_index_root_page`
  are not usable against a `Catalog` built this way, since there is no
  index catalog heap backing it; `create_index` remains the way to
  register an index that needs one.
- `Catalog::open(buffer_pool, txn_id) -> Result<Self, CatalogError>` -
  opens the catalog backed by `buffer_pool`: finds (or provisions) its
  persisted table heap and loads every `TableInfo` row from it into
  memory, then - if an index catalog heap has ever been provisioned (via
  `buffer_pool.index_catalog_first_page()`) - loads every `IndexInfo` the
  same way, discarding each row's `root_page_id` rather than caching it
  (see "No in-memory root-page cache" above). If no index has ever been
  created, the index catalog heap is left unprovisioned rather than
  eagerly created. `txn_id` is the transaction the catalog's own bootstrap
  heap allocation (the first time a database is ever opened) is
  attributed to; the caller is responsible for committing it, so that this
  bootstrap write is a real, committed transaction rather than one crash
  recovery would undo as a loser.
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
- `Catalog::create_index(buffer_pool, txn_id, name, table_id, column_index) -> Result<&IndexInfo, CatalogError>` -
  registers a new single-column index: rejects a duplicate `name`
  (`CatalogError::IndexAlreadyExists`), allocates a fresh `IndexId` and an
  empty `storage::btree::BTreeIndex` (via `BTreeIndex::create`), and
  persists its catalog row with that fresh tree's initial root page. Does
  not itself populate the index from existing rows - `engine::Database`'s
  `CREATE INDEX` handling does that afterward, the same way it would for
  any row `executor::InsertExecutor` adds later.
- `Catalog::index_for_column(table_id, column_index) -> Option<&IndexInfo>` -
  an in-memory-only lookup (no I/O) for whether an index exists on a given
  table's column; what `planner::optimizer::IndexScanRule` calls to decide
  whether a predicate can be rewritten into an index scan.
- `Catalog::indexes_for_table(table_id) -> impl Iterator<Item = &IndexInfo>` -
  every index covering `table_id`, in-memory-only; what
  `executor::InsertExecutor` iterates to maintain every index on a table as
  a row is inserted.
- `Catalog::index_root_page(buffer_pool, index_id) -> Result<PageId, CatalogError>` -
  the index's *current* root page, read fresh from the index catalog heap
  every call rather than cached (see "No in-memory root-page cache"
  above). `CatalogError::IndexNotFound` if the row is somehow missing (an
  internal-invariant violation, not expected from any correct caller) or
  `CatalogError::Corrupt` if the index catalog heap was never bootstrapped
  at all (also only reachable via an invariant violation - any `index_id`
  a caller has in hand came from the in-memory registry, which is only
  populated once the heap exists).
- `Catalog::update_index_root_page(buffer_pool, txn_id, index_id, new_root) -> Result<(), CatalogError>` -
  rewrites the index's catalog row with `new_root`: finds the current row
  (same scan as `index_root_page`) and its `Rid`, deletes it, and inserts
  the re-encoded replacement - `storage::heap::TableHeap` has no in-place
  update, and a row's `Rid` isn't stable across this rewrite, which is
  exactly why it isn't cached anywhere. The caller (`executor::InsertExecutor`,
  or `engine::Database`'s `CREATE INDEX` population step) is responsible
  for calling this whenever `BTreeIndex::insert` changes
  `BTreeIndex::root_page_id()` from what it was before the call.
- `Catalog::ensure_catalog_heap(buffer_pool, txn_id) -> Result<PageId, CatalogError>` -
  private. Returns the table catalog heap's root page, provisioning one
  (and recording it in the database header) if this is the first call
  against a catalog that didn't come from `open`.
- `Catalog::ensure_index_catalog_heap(buffer_pool, txn_id) -> Result<PageId, CatalogError>` -
  private. The same provisioning as `ensure_catalog_heap`, for the index
  catalog heap and `storage::disk::header::INDEX_CATALOG_FIRST_PAGE_RANGE`
  instead.

## Usage Example

```rust
use catalog::Catalog;

let mut catalog = Catalog::open(&buffer_pool, bootstrap_txn)?;
catalog.create_table(&buffer_pool, txn_id, "users", schema)?;
let info = catalog.get_table("users")?;

let index = catalog.create_index(&buffer_pool, txn_id, "idx_users_id", info.table_id, 0)?;
let root = catalog.index_root_page(&buffer_pool, index.index_id)?;
```

`engine::Database` holds one `Catalog` for the lifetime of an open
database, built once via `open` at startup and rebuilt wholesale via
`open` again on an explicit `ROLLBACK` (`engine::database.MD`), rather
than trying to undo its in-memory effects surgically - which is exactly
why nothing in `Catalog` may cache mutable state that only a reload could
keep consistent with disk (see "No in-memory root-page cache" above): an
*autocommit* statement's abort path does not reload the catalog, only
undoes the failed statement's own WAL-logged writes.
`tests/bootstrap_atomicity.rs` sweeps every crash point across that very
first `open` to check the catalog's bootstrap heap allocation and the
header pointer to it become durable together or not at all.
