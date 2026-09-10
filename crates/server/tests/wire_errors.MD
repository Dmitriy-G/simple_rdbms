# wire_errors

`server::wire`'s error reporting and `ReadyForQuery` transaction-status
tracking (M13.2 subtask 4), exercised end to end with
`tokio_postgres::Client::simple_query` against a real
`tokio::net::TcpListener` served by `server::wire::serve`, the same
harness shape as `wire_startup.rs` and `wire_simple_query.rs`. Each test
opens its own `tempfile`-backed `engine::Database`, connects once, and
asserts on the `tokio_postgres::Error`/`DbError` a failing statement
produces, since `tokio_postgres` itself refuses to keep going with a
connection whose transaction is in an unexpected state - a wrong
`ReadyForQuery` status surfaces there as the next statement behaving
unexpectedly, not as a decodable assertion, so these tests instead assert
on the SQLSTATE each statement gets and on whether the *next* statement
succeeds or fails, which is the observable a client actually depends on.

## Key Components

- `connect(db) -> tokio_postgres::Client` - shared test helper, identical
  to `wire_simple_query.rs`'s: binds an ephemeral `127.0.0.1:0` listener,
  spawns `server::wire::serve` on it, connects with `tokio_postgres`, and
  spawns the connection's background driver future without awaiting it.
- `sqlstate(err) -> Option<&str>` - pulls the five-character SQLSTATE out
  of a `tokio_postgres::Error` via `as_db_error()?.code().code()`; `None`
  means the error carried no `DbError` at all (the wire layer sent
  something other than an `ErrorResponse`), which every assertion here
  compares against `Some("...")` rather than unwrapping so the failure
  message on a mismatch names what was actually received.
- `an_undefined_table_reports_the_real_sqlstate_and_leaves_the_connection_usable` -
  `SELECT * FROM missing` outside any explicit transaction fails with
  `42P01` (`common::SqlState::UNDEFINED_TABLE`), and a following
  `CREATE TABLE` still succeeds - proving an autocommit statement's own
  failure never leaves the connection in the failed-transaction state,
  matching `engine::runtime`'s autocommit statements never touching
  `session.txn_slot`.
- `a_failed_statement_inside_a_transaction_fails_until_rollback` - inside
  an explicit `BEGIN`, a failing `SELECT` reports `42P01` and moves the
  connection into the failed-transaction state; the next statement is
  rejected with `25P02` (`common::SqlState::IN_FAILED_SQL_TRANSACTION`)
  without ever reaching the engine's own table lookup; `ROLLBACK` still
  succeeds, and a `SELECT` afterward proves the connection is usable
  again.

## Usage Example

```rust
let db = Arc::new(Database::open(DbConfig::new(db_path)).unwrap());
let client = connect(db).await;

let err = client.simple_query("SELECT * FROM missing").await.unwrap_err();
assert_eq!(sqlstate(&err), Some("42P01"));
```

Run via `cargo test -p server --test wire_errors`. Together with
`wire_simple_query.rs`, this is the automated proof that M13.2 subtask
4's `to_pg_error` and `Response::TransactionStart`/`TransactionEnd`
wiring in `crates/server/src/wire.rs` behave the way `wire.MD` describes,
not just that a successful statement round-trips.
