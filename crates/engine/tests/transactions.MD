# transactions

`BEGIN`/`COMMIT`/`ROLLBACK` against a real (tempfile-backed) database:
explicit transactions actually group statements atomically, nesting and
stray `COMMIT`/`ROLLBACK` are rejected, a rolled-back `CREATE TABLE`
really leaves no table (not just an empty heap), and autocommit still
holds for bare statements outside any explicit transaction.

## Key Components

- `open(dir) -> Database` - opens a `Database` at `dir.join("test.db")`.
- `rows_of(result) -> Vec<Vec<Value>>` - unwraps a `ResultSet::Rows` into
  each row's values, panicking on `RowsAffected`.
- `rollback_of_inserts_leaves_the_table_empty` - two inserts inside a
  `BEGIN`/`ROLLBACK` leave the table with no rows.
- `commit_of_inserts_keeps_them_and_survives_a_reopen` - two inserts
  inside a `BEGIN`/`COMMIT`, followed by a clean close and reopen, still
  show both rows.
- `rolled_back_create_table_leaves_no_table_and_the_name_is_reusable_with_a_different_schema`
  - a `CREATE TABLE` inside a rolled-back transaction leaves the table
  name entirely unregistered; recreating it with a different schema
  afterward succeeds, proving the catalog's in-memory map was actually
  reloaded, not just the heap page rolled back underneath a stale
  `tables_by_name` entry.
- `a_select_inside_an_explicit_transaction_does_not_end_it` - a `SELECT`
  run mid-transaction does not implicitly commit: a second `BEGIN`
  issued afterward still errors, since if the `SELECT` had ended the
  transaction that second `BEGIN` would have succeeded instead.
- `nested_begin_is_an_error` - a second `BEGIN` while one is already open
  is rejected.
- `commit_with_no_active_transaction_is_an_error` - `COMMIT` with no open
  transaction is rejected.
- `rollback_with_no_active_transaction_is_an_error` - `ROLLBACK` with no
  open transaction is rejected.
- `a_bare_insert_outside_any_transaction_is_durable_once_execute_returns`
  - an autocommit `INSERT` (no explicit `BEGIN`) is durable as soon as
  `execute` returns, even if the database is then dropped without calling
  `close`: only what `execute` already made durable (via
  `TransactionManager::commit`'s force-flush) may survive a drop without
  `close`, and that's still everything this test expects back.

## Usage Example

```rust
let dir = tempfile::tempdir().expect("create temp dir");
let mut db = open(&dir);
db.execute("CREATE TABLE t (a INTEGER)").expect("create table");
db.execute("BEGIN").expect("begin");
db.execute("INSERT INTO t VALUES (1)").expect("insert");
db.execute("ROLLBACK").expect("rollback");
assert_eq!(rows_of(db.execute("SELECT * FROM t").expect("select")), Vec::new());
```

Run as part of `cargo test --workspace`; these exercise `Database::
handle_begin`/`handle_commit`/`handle_rollback` and the autocommit path
through `Database::execute` entirely through the public `execute` API,
the same surface `cli`'s REPL drives.
