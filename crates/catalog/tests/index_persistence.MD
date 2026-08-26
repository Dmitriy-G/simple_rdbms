# index_persistence

Integration tests for the index half of the catalog - `Catalog::create_index`,
`index_for_column`, `indexes_for_table`, `index_root_page`, and
`update_index_root_page` - in the same reopen-and-verify style
`tests/persistence.rs` uses for tables.

## Key Components

- `open_pool(path) -> Result<BufferPool, Box<dyn Error>>` - the same
  tempfile-backed `BufferPool` construction `tests/persistence.rs` uses.
- `users_schema() -> Schema` - a two-column `(id INTEGER, email TEXT)`
  schema shared by every test here.
- `an_index_survives_reopen_with_its_identity_and_root_page_intact` - the
  test task.MD calls out as the one to write first: creates a table and an
  index on it, flushes, drops the `BufferPool`, reopens against a fresh
  one, and confirms `index_for_column`/`index_root_page`/`indexes_for_table`
  all still find it with the same identity and a real (non-sentinel) root
  page - the round trip a naive in-memory-only index registry would fail.
- `duplicate_index_name_is_rejected` - mirrors
  `tests/persistence.rs::duplicate_table_name_is_rejected` for
  `CatalogError::IndexAlreadyExists`.
- `a_root_page_update_survives_reopen` - calls `update_index_root_page`
  with a different `PageId` (standing in for what a real root split would
  produce - this test only exercises the catalog row rewrite, not
  `storage::btree::BTreeIndex` itself, which `crates/storage/tests/btree.rs`
  and `crates/executor/tests/` cover), confirms `index_root_page` reflects
  the change immediately, flushes, reopens, and confirms the new value -
  not the value at creation time - is what `Catalog::open` reloads. This is
  what proves `catalog.MD`'s "no in-memory root-page cache" design actually
  persists a root-page change durably despite never caching it.

## Usage Example

```rust
let pool = open_pool(&path)?;
let mut catalog = Catalog::open(&pool, TXN)?;
let table = catalog.create_table(&pool, TXN, "users", users_schema())?;
let index = catalog.create_index(&pool, TXN, "idx_users_id", table.table_id, 0)?;
let root = catalog.index_root_page(&pool, index.index_id)?;
```

Run as part of `cargo test --workspace`, or just this file with
`cargo test -p catalog --test index_persistence`. Unlike the table
catalog heap, the index catalog heap is never eagerly provisioned by
`Catalog::open` - only `create_index` calls `ensure_index_catalog_heap` -
so `tests/bootstrap_atomicity.rs`'s crash-injection sweep, which only
exercises `open`, does not cover its bootstrap; `crates/engine/tests/`
covers a `CREATE INDEX` that fails or is rolled back leaving no orphaned
index or pages, via ordinary transaction abort rather than simulated
crash injection.
