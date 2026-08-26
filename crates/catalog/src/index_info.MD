# index_info

Everything the rest of the engine needs to know about one B+tree index's
identity: which index it is, which table and column it covers. Deliberately
does not carry `root_page_id` - see `catalog.MD`'s "no in-memory root-page
cache" note for why that field would go stale across an aborted statement
and is instead always read fresh from the index catalog heap via
`Catalog::index_root_page`.

## Key Components

- `IndexInfo` - the index struct. `index_id` is the index's stable identity
  within the catalog. `name` is unique within the catalog. `table_id` and
  `column_index` name the single column this index covers (multi-column
  indexes are not supported yet).
- `IndexInfo::new(index_id, name, table_id, column_index) -> Self` - builds
  an index's catalog identity.

## Usage Example

```rust
use catalog::IndexInfo;

let info = IndexInfo::new(index_id, "idx_users_email", table_id, 2);
```

`Catalog::create_index` builds an `IndexInfo` from a fresh `IndexId` and
stores it in the catalog's in-memory index; `Catalog::open` rebuilds the
same `IndexInfo`s by decoding them back out of the index catalog's
persisted heap, discarding each row's `root_page_id` field rather than
caching it (`catalog.MD`).
