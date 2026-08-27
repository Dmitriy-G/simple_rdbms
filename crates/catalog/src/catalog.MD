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

**Root-page caching.** `IndexInfo` (`index_info.MD`) caches its `root_page_id`
in a `Cell`, plus the two pieces of bookkeeping needed to update it in
O(1): `catalog_rid` (its row's `Rid` in the index catalog heap, stable for
the index's whole lifetime - see below) and `root_page_id_offset` (the
byte offset of `root_page_id` within that row's own encoding).
`index_root_page` is a pure in-memory read; `update_index_root_page`
rewrites just the changed 4 bytes of the durable row via
`storage::heap::TableHeap::update_tuple_in_place` and updates the cache in
the same call, so the two can never observably diverge *within* one
`Catalog` instance. Neither method needs `&mut self` - `update_index_root_page`
mutates the `IndexInfo`'s `Cell` and writes through `&BufferPool`, matching
`storage::buffer::BufferPool::set_catalog_first_page`'s existing `&self`
pattern - which is what lets `executor::InsertExecutor` (holding only a
shared `&Catalog`) call it directly.

This cache is only sound because **every abort path reloads the catalog
from disk, not just an explicit `ROLLBACK`.** `storage::btree::BTreeIndex::insert`
mutates `root_page_id` on a split, and if the *rest* of that statement
then fails, WAL undo reverts the durable index-catalog row - if the cache
survived that revert, `index_root_page` would keep returning the
now-wrong, post-split value. Earlier (pre-caching) design deliberately
avoided this by never caching anything; now that a cache exists,
`engine::Database` reloads `self.catalog` wholesale after *every* abort -
autocommit failure, an explicit `ROLLBACK`, and a `COMMIT` that turns into
a rollback because the transaction was already aborted - not only the
`ROLLBACK` statement, which was the only path that reloaded before (see
`engine::database.MD`'s `Database::reload_catalog`). Since `Catalog::open`
builds every `IndexInfo` from scratch, reading `root_page_id`/`catalog_rid`/
`root_page_id_offset` fresh off the current (post-undo) heap contents,
this reload is what makes caching safe: there is nothing surgical to get
wrong, only "throw the old `Catalog` away, build a new one." The same
applies across a process restart - `storage::recovery::recover` always
runs before `Catalog::open` (`engine::database.rs`'s `open_with_managers`),
so a cache built by `open` after recovery already reflects every completed
redo and undo.

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
  `storage::btree::BTreeIndex`. `index_root_page` works against a `Catalog`
  built this way (it only reads the in-memory cache each `IndexInfo`
  already carries), but `update_index_root_page` does not - it needs a
  real index catalog heap to write through, which this constructor never
  provisions; `create_index` remains the way to register an index that
  needs one.
- `Catalog::open(buffer_pool, txn_id) -> Result<Self, CatalogError>` -
  opens the catalog backed by `buffer_pool`: finds (or provisions) its
  persisted table heap and loads every `TableInfo` row from it into
  memory, then - if an index catalog heap has ever been provisioned (via
  `buffer_pool.index_catalog_first_page()`) - loads every `IndexInfo` the
  same way, caching each row's `root_page_id`/`Rid`/offset fresh from what
  it just read (see "Root-page caching" above). If no index has ever been
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
  empty `storage::btree::BTreeIndex` (via `BTreeIndex::create`), encodes
  and inserts its catalog row (capturing the `Rid` it lands at and the
  byte offset of `root_page_id` within it), and builds the `IndexInfo`
  from all of that in one shot. Does not itself populate the index from
  existing rows - `engine::Database`'s `CREATE INDEX` handling does that
  afterward, the same way it would for any row `executor::InsertExecutor`
  adds later.
- `Catalog::index_for_column(table_id, column_index) -> Option<&IndexInfo>` -
  an in-memory-only lookup (no I/O) for whether an index exists on a given
  table's column; what `planner::optimizer::IndexScanRule` calls to decide
  whether a predicate can be rewritten into an index scan.
- `Catalog::indexes_for_table(table_id) -> impl Iterator<Item = &IndexInfo>` -
  every index covering `table_id`, in-memory-only; what
  `executor::InsertExecutor` iterates to maintain every index on a table as
  a row is inserted.
- `Catalog::index_root_page(index_id) -> Result<PageId, CatalogError>` -
  the index's *current* root page, read from the in-memory cache in O(1) -
  no `BufferPool` needed at all. `CatalogError::IndexNotFound` if the
  index is somehow missing from the in-memory registry (an
  internal-invariant violation, not expected from any correct caller,
  since any `index_id` a caller has in hand came from that same registry).
- `Catalog::update_index_root_page(buffer_pool, txn_id, index_id, new_root) -> Result<(), CatalogError>` -
  rewrites the index's catalog row with `new_root` in place, via
  `storage::heap::TableHeap::update_tuple_in_place` at the index's cached
  `catalog_rid`/`root_page_id_offset` - no scan, and (unlike the
  delete-then-reinsert this replaced) the row's `Rid` never changes, so
  every future call can keep using the same cached one. Updates the
  `IndexInfo`'s cached `root_page_id` only *after* the durable write
  succeeds. The caller (`executor::InsertExecutor`, or `engine::Database`'s
  `CREATE INDEX` population step) is responsible for calling this whenever
  `BTreeIndex::insert` changes `BTreeIndex::root_page_id()` from what it
  was before the call.
- `Catalog::ensure_catalog_heap(buffer_pool, txn_id) -> Result<PageId, CatalogError>` -
  private. Returns the table catalog heap's root page, provisioning one
  (and recording it in the database header) if this is the first call
  against a catalog that didn't come from `open`.
- `Catalog::ensure_index_catalog_heap(buffer_pool, txn_id) -> Result<PageId, CatalogError>` -
  private. The same provisioning as `ensure_catalog_heap`, for the index
  catalog heap and `storage::disk::header::INDEX_CATALOG_FIRST_PAGE_RANGE`
  instead.
- `Catalog::get_index_by_id(index_id) -> Result<&IndexInfo, CatalogError>` -
  private. The `indexes_by_id` -> `indexes_by_name` double lookup
  `index_root_page` and `update_index_root_page` both need, factored out
  so the two share one `CatalogError::IndexNotFound` message rather than
  each formatting their own.

## Usage Example

```rust
use catalog::Catalog;

let mut catalog = Catalog::open(&buffer_pool, bootstrap_txn)?;
catalog.create_table(&buffer_pool, txn_id, "users", schema)?;
let info = catalog.get_table("users")?;

let index = catalog.create_index(&buffer_pool, txn_id, "idx_users_id", info.table_id, 0)?;
let root = catalog.index_root_page(index.index_id)?;
```

`engine::Database` holds one `Catalog` for the lifetime of an open
database, built once via `open` at startup and rebuilt wholesale
(`Database::reload_catalog`, `engine::database.MD`) after every abort -
not only an explicit `ROLLBACK` - rather than trying to undo its
in-memory effects (including the `root_page_id` cache) surgically; see
"Root-page caching" above for why that reload is load-bearing now that a
cache exists at all. `tests/bootstrap_atomicity.rs` sweeps every crash
point across that very first `open` to check the catalog's bootstrap heap
allocation and the header pointer to it become durable together or not at
all.
