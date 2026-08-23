# data_type

The set of column types the engine understands. A `catalog::Column`
carries one of these plus a nullability flag; nullability itself is not
part of the type — a `NULL` is a `Value::Null` that can appear in any
nullable column, not a distinct `DataType`.

## Key Components

- `DataType` - the enum. `Boolean` is a single-byte boolean, `true` or
  `false`. `Integer` is a 4-byte signed integer. `BigInt` is an 8-byte
  signed integer. `Double` is an 8-byte IEEE-754 floating point number.
  `Varchar(u32)` is a UTF-8 string with an inclusive maximum length in
  bytes, mirroring SQL `VARCHAR(n)`.
- `DataType::fixed_width(&self) -> Option<usize>` - the on-disk width in
  bytes of a fixed-size type, or `None` for a variable-length type like
  `Varchar`, which is stored out-of-line with a length prefix. Not yet
  called by anything in this workspace; it is here as the type's own
  answer to "how wide is this on disk," ready for the fixed-width layout
  optimization it was written for once that lands.

## Usage Example

```rust
use types::DataType;

let column_type = DataType::Varchar(255);
assert_eq!(column_type.fixed_width(), None);
assert_eq!(DataType::Integer.fixed_width(), Some(4));
```

`Value::data_type` returns one of these for every non-`Null` value, and
`Tuple::encode`/`Tuple::decode` (see `tuple.MD`) switch on a column's
`DataType` to decide how many bytes to read or write and whether they are
fixed-width or length-prefixed.
