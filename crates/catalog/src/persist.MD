# persist

Encodes and decodes `TableInfo` rows for the catalog's own persisted table
heap, using `types`'s tuple encoder against a fixed row schema:
`(table_id INTEGER, name TEXT, first_page INTEGER, schema_blob TEXT)`.

`schema_blob` is this module's own hand-rolled encoding of a `Schema` —
column count, then per column its name, type tag, `Varchar` length (if
applicable), and nullability — hex-encoded so it round-trips safely
through a `TEXT` column, which requires valid UTF-8. No serde.

## Key Components

- `CATALOG_ROW_SCHEMA` - the fixed column types of a catalog row, in the
  order fields are encoded. The `Varchar` length parameter is not
  meaningful for decoding (only the length-prefixed bytes matter), so it's
  left at 0.
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
index.
