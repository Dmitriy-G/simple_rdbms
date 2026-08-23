# tuple

An ordered row of values, in schema column order, plus the machinery that
converts one to and from its on-disk byte representation. `Tuple` is the
in-memory form the executor passes between operators; `storage` deals only
in the encoded byte form `Encode`/`Tuple::decode` produce and consume, so
this module is the one place that boundary is crossed.

The on-disk layout is a leading null bitmap (one bit per column, ordinal
order, rounded up to whole bytes) followed by each non-null column's
value back to back: fixed-width types write their raw little-endian bytes
in place, and `Varchar` writes a 4-byte little-endian length prefix
followed by its UTF-8 bytes. A null column contributes nothing to the
value bytes at all — only its bitmap bit is set — which is why decoding
needs the schema's column types up front rather than being able to walk
the buffer on its own.

## Key Components

- `Tuple` - the row. Wraps a `Vec<Value>` in column order.
- `Tuple::new(values: Vec<Value>) -> Self` - builds a tuple from
  already-evaluated values.
- `Tuple::values(&self) -> &[Value]` - borrows the tuple's values in
  column order.
- `Tuple::decode(buf: &[u8], column_types: &[DataType]) -> Result<Tuple, TupleError>` -
  reconstructs a tuple from its on-disk byte representation, given the
  declared type of each column in order. Unlike `Encode`, decoding needs
  that external type information: the encoded bytes for, say, a `Boolean`
  and the length prefix of a `Varchar` are not self-describing on their
  own, so there is no way to know where one column's bytes end and the
  next begins without already knowing what type each one is.
- `take(buf, offset, len) -> Result<&[u8], TupleError>` - private. Returns
  the `len`-byte slice of `buf` starting at `offset`, or a
  `TupleError::Truncated` if `buf` does not have that many bytes; every
  read `Tuple::decode` performs goes through this so a truncated buffer
  (a torn or short read) is reported instead of panicking on an
  out-of-bounds slice.
- `TupleError` - errors raised while decoding a tuple's on-disk bytes.
  `Truncated { expected, found }` means the buffer ended before every
  column described by the schema could be read out of it. `InvalidUtf8`
  means a `Varchar` column's bytes were not valid UTF-8.
- `Encode` - converts a value into its on-disk byte representation, as
  stored inside a slotted page. Implemented per `DataType` so the storage
  layer can lay fixed-width columns out contiguously and variable-length
  columns out-of-line with a length prefix. `Encode::encode(&self, buf:
  &mut Vec<u8>)` appends this value's encoded bytes to `buf`.
- `Decode` - the generic inverse of `Encode`: `Decode::decode(buf) ->
  Result<(Self, usize), Self::Error>` reads a value out of `buf`,
  returning the value and the number of bytes consumed. Declared for
  symmetry with `Encode`, but nothing in this workspace implements it —
  `Tuple`'s own decoding needs the external column-type information
  described above, which `Decode::decode`'s single-buffer signature has
  no room for, so `Tuple::decode` is an inherent method instead.
- `impl Encode for Tuple` - the only `Encode` implementation here. Writes
  the null bitmap first (resizing `buf` to make room for it up front),
  then each non-null value's bytes in column order, setting that column's
  bitmap bit instead of writing anything when the value is `Value::Null`.

## Usage Example

```rust
use types::{Encode, Tuple, Value};

let tuple = Tuple::new(vec![Value::Integer(1), Value::Varchar("hi".to_string())]);
let mut buf = Vec::new();
tuple.encode(&mut buf);

let column_types = [types::DataType::Integer, types::DataType::Varchar(255)];
let round_tripped = Tuple::decode(&buf, &column_types)?;
assert_eq!(round_tripped.values(), tuple.values());
```

`catalog::persist::encode_table_info` builds a `Tuple` from a
`TableInfo`'s fields and calls `encode` on it to get the bytes it writes
into the catalog's own heap; `executor::InsertExecutor::next` does the
same for every row an `INSERT` evaluates before handing the bytes to
`TableHeap::insert_tuple`. Reading back goes through `Tuple::decode`
directly: `executor::SeqScanExecutor::next` decodes each slot a table
scan reads using the scanned table's column types, and
`catalog::persist::decode_table_info` decodes a catalog row the same way
against the catalog's own fixed schema.
