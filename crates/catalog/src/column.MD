# column

The definition of a single table column: its name, declared type, and
whether `NULL` is a legal value for it.

## Key Components

- `Column` - the column struct. `name` is unique within its table.
  `data_type` is the column's declared SQL type. `nullable` says whether
  `NULL` is a legal value for this column.
- `Column::new(name, data_type, nullable) -> Self` - builds a column
  definition.

## Usage Example

```rust
use catalog::Column;
use types::DataType;

let id = Column::new("id", DataType::Integer, false);
```

`Schema::new` takes an ordered `Vec<Column>` to describe a table's tuple
shape; `persist::encode_schema_blob` walks that same list to write each
column's name, type tag, and nullability into a table's persisted catalog
row.
