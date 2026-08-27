# index_info

Everything the rest of the engine needs to know about one B+tree index:
its identity (which index, which table and column) plus enough cached
bookkeeping to update its root page after a split in O(1) - no heap scan,
no page fetch - while keeping the durable index-catalog row and the
in-memory cache from ever diverging. See `catalog.MD`'s "root-page
caching" section for the full design and, critically, why it is only
safe given how `engine::Database` now reloads the catalog on every abort
path, not just an explicit `ROLLBACK`.

## Key Components

- `IndexInfo` - the index struct. `index_id` is the index's stable
  identity within the catalog. `name` is unique within the catalog.
  `table_id` and `column_index` name the single column this index covers
  (multi-column indexes are not supported yet). `catalog_rid` (private) is
  the `Rid` of this index's own row in the index-catalog heap - stable for
  the index's whole lifetime, since `Catalog::update_index_root_page`
  rewrites that row's bytes in place (`persist.MD`) rather than
  delete-then-reinsert, which would give it a new `Rid` every root split.
  `root_page_id_offset` (private) is the byte offset of the `root_page_id`
  field *within* that row's own encoded tuple - fixed for a given `name`
  (`persist.rs`'s `decode_index_row` derives it as `bytes.len() - 4`,
  since `root_page_id` is the row schema's last, fixed-width field) and
  cached here so `Catalog::update_index_root_page` never has to
  re-decode the row to find where to write. `root_page_id` (private) is a
  `Cell<PageId>` - the one field that changes after construction, on every
  root split; `Cell` rather than a plain field because
  `Catalog::update_index_root_page` only ever has `&Catalog` (called from
  `executor::InsertExecutor`, which never gets `&mut Catalog` - see
  `catalog.MD`), so mutating it needs interior mutability.
- `IndexInfo::new(index_id, name, table_id, column_index, root_page_id, catalog_rid, root_page_id_offset) -> Self` -
  builds an index's full catalog entry, including its current root page
  and the two pieces of bookkeeping needed to update it later.
- `IndexInfo::root_page_id(&self) -> PageId` - the *current* root page,
  read from the cache in O(1).
- `IndexInfo::catalog_rid(&self) -> Rid` - `pub(crate)`. The `Rid` of this
  index's row in the index-catalog heap; only `Catalog` needs this.
- `IndexInfo::root_page_id_offset(&self) -> usize` - `pub(crate)`. The
  byte offset described above; only `Catalog` needs this.
- `IndexInfo::set_root_page_id(&self, new_root)` - `pub(crate)`. Updates
  the cached root page. Not `pub`: the only sanctioned way to change an
  index's root page is `Catalog::update_index_root_page`, which calls this
  *after* successfully rewriting the durable row, never before and never
  as a substitute for it - exposing this publicly would let a caller
  desync the cache from disk by calling it directly.

## Usage Example

```rust
use catalog::IndexInfo;

let info = IndexInfo::new(index_id, "idx_users_email", table_id, 2, root_page_id, catalog_rid, offset);
assert_eq!(info.root_page_id(), root_page_id);
```

`Catalog::create_index` builds an `IndexInfo` from a fresh `IndexId`,
right after inserting its row into the index-catalog heap (so it has a
real `catalog_rid` and offset to give it), and stores it in the catalog's
in-memory index; `Catalog::open` rebuilds the same `IndexInfo`s by
decoding them back out of the index catalog's persisted heap, computing
each row's `catalog_rid`/`root_page_id_offset` fresh from what it just
read - a `Catalog` built via `Catalog::open` never carries a stale cache,
because it never carries anything from a previous `Catalog` at all.
