# transactions

`BEGIN`/`COMMIT`/`ROLLBACK` against a real (tempfile-backed) database:
explicit transactions actually group statements atomically, nesting and
stray `COMMIT`/`ROLLBACK` are rejected, a rolled-back `CREATE TABLE`
really leaves no table (not just an empty heap), autocommit still holds
for bare statements outside any explicit transaction, and a statement
that fails inside an explicit transaction aborts it - Postgres semantics,
per CLAUDE.md's error-handling section.

## Key Components

- `open(dir) -> Database` - opens a `Database` at `dir.join("test.db")`.
  Marked `#[cfg(test)]` so `clippy.toml`'s `allow-expect-in-tests` applies
  to its `.expect(...)` call - see `storage/tests/recovery_logging.MD` for
  why a free function in a `tests/` file needs the attribute spelled out
  explicitly.
- `rows_of(result) -> Vec<Vec<Value>>` - unwraps a `ResultSet::Rows` into
  each row's values, panicking on `RowsAffected`/`RolledBack`.
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
- `a_failed_statement_inside_begin_aborts_the_transaction_until_rollback`
  - a `SELECT` against a missing table inside `BEGIN` fails with
  `SqlState::UNDEFINED_TABLE`; every statement issued afterward, even a
  well-formed one, is then rejected with `SqlState::IN_FAILED_SQL_TRANSACTION`
  until `ROLLBACK`, after which a fresh `BEGIN` and ordinary statements
  work again.
- `commit_of_an_aborted_transaction_rolls_back_and_reports_it` - `COMMIT`
  against a transaction already aborted by a failed statement is not
  itself an error: it performs the deferred rollback and returns
  `ResultSet::RolledBack` rather than the ordinary `RowsAffected(0)`, and
  the transaction's writes are gone afterward, same as an explicit
  `ROLLBACK` would leave them.

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
