# prepared_statements

End-to-end tests of `Database::describe` and `Database::execute_with_params`
(M13.3, `docs/ROADMAP.md`), against a real (tempfile-backed) database file.

## Key Components

- `open(dir) -> Database` - opens a `Database` at `dir.join("test.db")`.
  Marked `#[cfg(test)]` so `clippy.toml`'s `allow-expect-in-tests` applies
  to its `.expect(...)` call, mirroring `tests/integration.rs`'s own
  helper of the same name.
- `rows_and_columns_of(result) -> (Vec<String>, Vec<Vec<Value>>)` - unwraps
  a `ResultSet::Rows` into its columns and each row's values, panicking on
  `RowsAffected`/`RolledBack`.
- `describe_reports_parameter_and_column_types` - `describe` of `SELECT a,
  b FROM t WHERE a = $1` against `t(a INTEGER, b TEXT)` reports
  `param_types: [Some(Integer)]` and both result columns with their
  resolved types.
- `describe_leaves_no_transaction_or_lock_behind` - after `describe`,
  `Database::current_txn_id` reports no open transaction on the session,
  and an immediately following `CREATE INDEX` - which would block behind
  any lock `describe` might have left held - completes normally.
- `execute_with_params_inserts_and_selects_with_bound_values_in_autocommit` -
  a parameterized `INSERT` and a parameterized `SELECT`, each its own
  autocommit statement, round-trip bound `Integer`/`Varchar` values.
- `execute_with_params_works_inside_an_explicit_transaction` - the same
  parameterized `INSERT`, run between `BEGIN` and `COMMIT`, is visible
  afterward exactly like an autocommit one.
- `wrong_parameter_count_is_a_protocol_violation` - supplying two values
  for a statement with one `$1` placeholder returns
  `common::Error::ParameterCountMismatch { expected: 1, found: 2 }`,
  SQLSTATE `08P01`.
- `plain_execute_on_parameterized_sql_is_undefined_parameter` -
  `Database::execute` (not `execute_with_params`) on SQL containing `$1`
  returns `common::Error::UndefinedParameter { index: 1 }`, SQLSTATE
  `42P02` - proving a bare `execute` call is unaffected by
  `execute_with_params`'s substitution path.

## Usage Example

```rust
let mut db = Database::open(DbConfig::new("example.db"))?;
db.execute("CREATE TABLE t (a INTEGER)")?;
let description = db.describe("SELECT a FROM t WHERE a = $1")?;
db.execute_with_params("INSERT INTO t VALUES ($1)", &[Value::Integer(1)])?;
```

Run as part of `cargo test --workspace`; this is the crate's coverage for
the two new `Database` entry points `database.MD`/`runtime.MD` document,
complementing `tests/integration.rs`'s coverage of plain `execute`.
