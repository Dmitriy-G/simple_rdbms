# schema

An ordered list of columns describing the shape of a table's tuples.
Column order here is the same order values appear in a `types::Tuple` and
in the tuple's encoded on-disk bytes.

## Key Components

- `Schema` - the schema struct: an ordered `Vec<Column>`.
- `Schema::new(columns) -> Self` - builds a schema from an ordered list of
  columns.
- `Schema::columns() -> &[Column]` - the schema's columns, in tuple order.
- `Schema::column_index(name) -> Option<usize>` - the ordinal position of
  the column named `name`, or `None` if no such column exists.

## Usage Example

```rust
use catalog::{Column, Schema};
use types::DataType;

let schema = Schema::new(vec![
    Column::new("id", DataType::Integer, false),
    Column::new("name", DataType::Varchar(64), true),
]);
let idx = schema.column_index("name");
```

`TableInfo` carries one `Schema` per table; the planner's binder uses
`column_index` to resolve a column reference in a SQL statement to the
ordinal position an operator reads from a `types::Tuple`.
