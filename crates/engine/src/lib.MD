# lib

The database facade: the single entry point that wires the SQL, planner,
executor, catalog, storage, and transaction crates together behind one
`Database::execute(sql) -> ResultSet` call.

Every other crate in the workspace is a layer; `engine` is where the
layers are assembled. Nothing below this crate knows about any other
sibling (`sql` does not know about `executor`, `catalog` does not know
about `planner`); `engine` is the only place that does.

## Key Components

- `Database` - a thin, ergonomic wrapper around one session of the engine
  described below; opens, closes, and executes SQL against a single
  database with a public surface unchanged since before M13.1 split the
  engine out behind its own dispatch thread. See `database.MD`.
- `runtime` - private module. `EngineHandle`/`SessionHandle` and the
  message-passing protocol between them: the engine-wide state (catalog,
  buffer pool, transaction manager) and per-session state (the explicit
  transaction slot, its span, the statement counter) that used to live
  together on `Database` now live behind a dedicated dispatch thread
  reached by message passing - so that a multi-connection frontend can
  drive many sessions through one engine without sharing mutable state
  across real threads directly - with each statement itself run on
  `worker_pool::WorkerPool`'s fixed eight-thread pool rather than on the
  dispatch thread (M10.2, `docs/ROADMAP.md`), so two sessions' statements
  genuinely execute concurrently. `Database::connect` is that future
  arrived: every `crates/engine/tests/sessions.rs` test drives more than
  one session through the same engine today. See `runtime.MD`.
- `ResultSet` - the result of executing one SQL statement. See
  `result_set.MD`.
- `StatementDescription` - the result of `Database::describe`: a `$n`
  placeholder's inferred types and a `SELECT`'s result column shape,
  learned without executing anything. See `statement_description.MD`.
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
    ResultSet::Rows { columns, rows, .. } => { /* ... */ }
    ResultSet::RowsAffected(n) => { /* ... */ }
}
db.close()?;
```

`cli` is the only caller: it opens one `Database` for the process's
lifetime and calls `execute` once per line the REPL reads, printing
whichever `ResultSet` variant comes back.
