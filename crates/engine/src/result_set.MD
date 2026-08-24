# result_set

The result of executing one SQL statement. A query (`SELECT`) yields
`Rows`; a statement with no output rows of its own (`INSERT`, `CREATE
TABLE`) yields `RowsAffected` instead of an empty `Rows`, so a caller can
tell "no columns because this wasn't a query" apart from "a query that
matched nothing".

## Key Components

- `ResultSet` - `Rows { columns: Vec<String>, rows: Vec<Tuple> }` for a
  query's output (column names in projection order, and the rows produced
  in the order they were produced), or `RowsAffected(usize)` for a
  statement with no output rows: the number of rows it inserted, updated,
  or deleted (`0` for DDL like `CREATE TABLE`).
- `ResultSet::rows(columns, rows) -> Self` - builds a `Rows` result set
  from its output column names and rows.
- `ResultSet::rows_affected(count) -> Self` - builds a `RowsAffected`
  result set.

## Usage Example

```rust
use engine::ResultSet;

match db.execute("INSERT INTO t VALUES (1)")? {
    ResultSet::RowsAffected(n) => println!("{n} row(s) affected"),
    ResultSet::Rows { .. } => unreachable!("INSERT never yields Rows"),
}
```

`Database::execute_bound` constructs a `RowsAffected` for `CREATE TABLE`
and `INSERT`, and a `Rows` for `SELECT`; `cli` matches on the variant to
decide whether to print a row count or a table of results.
