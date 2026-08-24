# result_set

The result of executing one SQL statement. A query (`SELECT`) yields
`Rows`; a statement with no output rows of its own (`INSERT`, `CREATE
TABLE`) yields `RowsAffected` instead of an empty `Rows`, so a caller can
tell "no columns because this wasn't a query" apart from "a query that
matched nothing"; `COMMIT` issued against an already-aborted transaction
yields `RolledBack` instead of the ordinary `RowsAffected(0)`, so a caller
can tell the client asked to keep its work and got the opposite.

## Key Components

- `ResultSet` - `Rows { columns: Vec<String>, rows: Vec<Tuple> }` for a
  query's output (column names in projection order, and the rows produced
  in the order they were produced); `RowsAffected(usize)` for a statement
  with no output rows: the number of rows it inserted, updated, or deleted
  (`0` for DDL like `CREATE TABLE`, and for an ordinary `BEGIN`/`COMMIT`/
  `ROLLBACK`); `RolledBack` for the one case where a statement's visible
  outcome doesn't match what it asked for - see `database.MD`'s
  `handle_commit` entry.
- `ResultSet::rows(columns, rows) -> Self` - builds a `Rows` result set
  from its output column names and rows.
- `ResultSet::rows_affected(count) -> Self` - builds a `RowsAffected`
  result set.

## Usage Example

```rust
use engine::ResultSet;

match db.execute("COMMIT")? {
    ResultSet::RowsAffected(_) => println!("COMMIT"),
    ResultSet::RolledBack => println!("ROLLBACK"),
    ResultSet::Rows { .. } => unreachable!("COMMIT never yields Rows"),
}
```

`Database::execute_bound` constructs a `RowsAffected` for `CREATE TABLE`
and `INSERT`, and a `Rows` for `SELECT`; `Database::handle_begin`/
`handle_rollback` also return `RowsAffected(0)`, while `handle_commit`
returns `RolledBack` instead when the transaction it's ending was already
aborted. `cli` matches on the variant to decide whether to print a row
count, a table of results, or `ROLLBACK`.
