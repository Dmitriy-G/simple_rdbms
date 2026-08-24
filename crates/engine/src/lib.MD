# lib

The database facade: the single entry point that wires the SQL, planner,
executor, catalog, storage, and transaction crates together behind one
`Database::execute(sql) -> ResultSet` call.

Every other crate in the workspace is a layer; `engine` is where the
layers are assembled. Nothing below this crate knows about any other
sibling (`sql` does not know about `executor`, `catalog` does not know
about `planner`); `engine` is the only place that does.

## Key Components

- `Database` - owns the catalog, buffer pool, and transaction manager;
  opens, closes, and executes SQL against a single database. See
  `database.MD`.
- `ResultSet` - the result of executing one SQL statement. See
  `result_set.MD`.
- `build_executor` - lowers a `planner::PhysicalPlan` into an `executor`
  operator tree. Private to the crate. See `executor_factory.MD`.
- `DataType`, `Tuple`, `Value` - re-exported from `types`, so `cli` (which
  may depend only on `engine` and `common`, per the workspace's
  dependency-edge rules) can name the types `ResultSet`'s rows are made of
  without depending on `types` directly.

## Usage Example

```rust
use common::DbConfig;
use engine::{Database, ResultSet};

let mut db = Database::open(DbConfig::new("example.db"))?;
match db.execute("SELECT * FROM t")? {
    ResultSet::Rows { columns, rows } => { /* ... */ }
    ResultSet::RowsAffected(n) => { /* ... */ }
}
db.close()?;
```

`cli` is the only caller: it opens one `Database` for the process's
lifetime and calls `execute` once per line the REPL reads, printing
whichever `ResultSet` variant comes back.
