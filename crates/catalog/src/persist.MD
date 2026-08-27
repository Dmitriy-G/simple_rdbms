# persist

Encodes and decodes `TableInfo` rows for the catalog's own persisted table
heap, and `IndexInfo` rows for the index catalog's persisted heap, using
`types`'s tuple encoder against fixed row schemas:
`(table_id INTEGER, name TEXT, first_page INTEGER, schema_blob TEXT)` for a
table, `(index_id INTEGER, name TEXT, table_id INTEGER, column_index
INTEGER, root_page_id INTEGER)` for an index.

`schema_blob` is this module's own hand-rolled encoding of a `Schema` —
column count, then per column its name, type tag, `Varchar` length (if
applicable), and nullability — hex-encoded so it round-trips safely
through a `TEXT` column, which requires valid UTF-8. No serde.

## Key Components

- `CATALOG_ROW_SCHEMA` - the fixed column types of a table catalog row, in
  the order fields are encoded. The `Varchar` length parameter is not
  meaningful for decoding (only the length-prefixed bytes matter), so it's
  left at 0.
- `INDEX_CATALOG_ROW_SCHEMA` - the fixed column types of an index catalog
  row, same `Varchar`-length caveat as `CATALOG_ROW_SCHEMA`.
- `type_tag(data_type) -> u8` - private. Maps a `DataType` to the one-byte
  tag stored in a schema blob.
- `tag_to_type(tag, varchar_len) -> Result<DataType, CatalogError>` -
  private. The inverse of `type_tag`; `varchar_len` is only consulted for
  tag `4` (`Varchar`).
- `encode_schema_blob(schema) -> String` - private. Encodes a `Schema` as
  the hex string stored in a catalog row's `schema_blob` column.
- `decode_schema_blob(blob) -> Result<Schema, CatalogError>` - private.
  The inverse of `encode_schema_blob`.
- `encode_table_info(info) -> Vec<u8>` - crate-visible. Encodes `info` as
  a catalog row's tuple bytes.
- `decode_table_info(bytes) -> Result<TableInfo, CatalogError>` -
  crate-visible. Decodes a catalog row's tuple bytes back into a
  `TableInfo`.
- `encode_index_row(index_id, name, table_id, column_index, root_page_id) -> Vec<u8>` -
  crate-visible. Encodes an index catalog row's tuple bytes from primitive
  fields rather than an `&IndexInfo` - `Catalog::create_index` needs the
  encoded bytes (to get a `Rid` back from inserting them) *before* an
  `IndexInfo` can be built, since `IndexInfo::new` itself needs that `Rid`
  and the row's `root_page_id_offset` (see `index_info.MD`); taking
  primitives instead of `&IndexInfo` avoids that chicken-and-egg
  ordering entirely.
- `decode_index_row(bytes, catalog_rid) -> Result<IndexInfo, CatalogError>` -
  crate-visible. The inverse of `encode_index_row`, building a complete,
  ready-to-cache `IndexInfo` directly: `catalog_rid` is the caller's own
  (from `TableHeap::iter`, which already hands back a `Rid` alongside
  every row's bytes), and `root_page_id_offset` is derived as `bytes.len()
  - 4` - `root_page_id` is the row schema's last field, a fixed-width
  `Integer`, so its bytes are always exactly the last 4 bytes of the
  encoded tuple regardless of `name`'s own length (which is the only
  variable-width part of the row, and precedes `root_page_id`).
- `take(bytes, offset, len) -> Result<&[u8], CatalogError>` - private.
  Slices `bytes[offset..offset+len]`, converting an out-of-range slice
  into `CatalogError::Corrupt` instead of panicking.
- `to_hex(bytes) -> String` - private. Lowercase hex encoding.
- `from_hex(text) -> Result<Vec<u8>, CatalogError>` - private. The inverse
  of `to_hex`; rejects an odd-length string.
- `hex_digit(digit) -> Result<u8, CatalogError>` - private. Decodes one
  ASCII hex digit.

## Usage Example

```rust
let bytes = encode_table_info(&info);
let round_tripped = decode_table_info(&bytes)?;
assert_eq!(round_tripped, info);
```

`Catalog::create_table` calls `encode_table_info` before inserting a row
into the catalog's own `TableHeap`; `Catalog::open` calls `decode_table_info`
on every row it reads back out of that heap while rebuilding the in-memory
index. `Catalog::create_index` calls `encode_index_row` once, to build the
row it inserts when a new index is created; `Catalog::open` calls
`decode_index_row` on every row it reads back out of the index catalog
heap. Neither `encode_index_row` nor `decode_index_row` is called again
after that: a root split updates the cached `IndexInfo` and rewrites only
the row's `root_page_id` bytes in place
(`storage::heap::TableHeap::update_tuple_in_place`, see `catalog.MD`),
never re-encoding or re-decoding the whole row.
