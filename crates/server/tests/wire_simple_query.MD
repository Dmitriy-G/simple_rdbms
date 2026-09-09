# wire_simple_query

`server::wire`'s simple-query handling (M13.2 subtask 3), exercised end to
end with `tokio_postgres::Client::simple_query` against a real
`tokio::net::TcpListener` served by `server::wire::serve`, exactly as
`wire_startup.rs` does for the startup handshake. Each test opens its own
`tempfile`-backed `engine::Database`, connects once, and drives one or
more simple queries through it, asserting on the `tokio_postgres::SimpleQueryMessage`s
returned rather than on log output or internal state.

## Key Components

- `connect(db) -> tokio_postgres::Client` - shared test helper. Binds an
  ephemeral `127.0.0.1:0` listener, spawns `server::wire::serve` on it,
  connects with `tokio_postgres`, and spawns the connection's background
  driver future without awaiting it - the same pattern
  `wire_startup.rs`'s test uses, factored out because every test here
  needs it.
- `column_names(columns) -> Vec<&str>` - maps a `RowDescription`'s
  `tokio_postgres::SimpleColumn` slice to plain column names for
  assertions.
- `create_insert_and_select_round_trip_over_simple_query` - `CREATE TABLE`
  with a `BOOLEAN`, an `INTEGER` and a `TEXT` column, an `INSERT` of two
  rows, then a `SELECT *`; asserts the `RowDescription`'s column names,
  both rows' text-encoded values (`t`/`f` for the boolean), and the
  trailing `CommandComplete`'s row count.
- `an_empty_select_still_reports_its_row_description` - a `SELECT` against
  an empty table still returns a `RowDescription` naming its one column
  before the zero-row `CommandComplete`.
- `begin_and_commit_succeed_over_simple_query` - `BEGIN`, an `INSERT`,
  `COMMIT`, then a `SELECT` outside the transaction sees the inserted
  row - proving the transaction actually committed rather than merely
  that the wire messages succeeded.
- `explain_returns_a_query_plan_over_simple_query` - `EXPLAIN SELECT * FROM t`
  returns a `RowDescription` naming the single `QUERY PLAN` column and at
  least one plan-line row.

## Usage Example

```rust
let db = Arc::new(Database::open(DbConfig::new(db_path)).unwrap());
let client = connect(db).await;

client.simple_query("CREATE TABLE t (a INTEGER)").await.unwrap();
let select = client.simple_query("SELECT * FROM t").await.unwrap();
assert!(matches!(select.last(), Some(SimpleQueryMessage::CommandComplete(0))));
```

Run via `cargo test -p server --test wire_simple_query`. Together with
`wire_startup.rs`, this is the automated proof that M13.2's wire listener
speaks real PostgreSQL simple-query messages, not just the startup
handshake; the manual `psql` transcript in `crates/server/README.md`
(M13.2 subtask 6) is what proves the same thing against the compiled
binary and container image.
