# lib

The value system shared by the SQL layer, catalog, and storage engine: the
set of column types (`DataType`), the runtime values that populate tuples
(`Value`), and the traits used to encode/decode tuples to and from the
byte representation stored on disk. Sits just above `common` in the
dependency graph (see `docs/adr/0002-crate-splitting.md`), depending only
on it, so every crate that needs to talk about a row's shape or contents —
`catalog`, `sql`, `planner`, `executor`, and `storage`'s callers — shares
this one vocabulary instead of each defining its own.

## Key Components

- `DataType` - the set of column types the engine understands. See
  `data_type.MD`.
- `Value` - a runtime value: one cell of a tuple. See `value.MD`.
- `Tuple`, `Encode`, `Decode`, `TupleError` - an ordered row of values,
  the traits used to convert one to and from on-disk bytes, and the error
  that conversion can raise. See `tuple.MD`.

## Usage Example

```rust
use types::{Tuple, Value};

// storage returns a page slot's raw bytes; the executor decodes them
// against the table's column types to get a `Tuple` back:
let tuple = Tuple::decode(&bytes, &column_types)?;

// going the other way, encoding a freshly-built row before it's inserted:
let mut buf = Vec::new();
Tuple::new(vec![Value::Integer(1)]).encode(&mut buf);
```

`executor::SeqScanExecutor::next` decodes a `Tuple` from every heap page
slot it reads, and `executor::InsertExecutor::next` encodes one before
handing its bytes to `storage::heap::TableHeap::insert_tuple`;
`catalog`'s own row persistence (`catalog::persist::encode_table_info`/
`decode_table_info`) reuses the exact same `Tuple`/`Encode`/`Tuple::decode`
path rather than hand-rolling its own row format.
