# wire_set_show_reset

`server::wire`'s `SET`/`SHOW`/`RESET` interception (M13.2 subtask 5),
exercised end to end with `tokio_postgres::Client::simple_query` against a
real `tokio::net::TcpListener` served by `server::wire::serve`, the same
pattern `wire_simple_query.rs` and `wire_errors.rs` use. Each test opens
its own `tempfile`-backed `engine::Database`, connects once, and drives
the three keywords through it, asserting on the returned
`tokio_postgres::SimpleQueryMessage`s rather than on log output or
internal state.

## Key Components

- `connect(db) -> tokio_postgres::Client` - the same shared test helper
  `wire_simple_query.rs` and `wire_errors.rs` each carry their own copy
  of: binds an ephemeral `127.0.0.1:0` listener, spawns
  `server::wire::serve` on it, connects with `tokio_postgres`, and spawns
  the connection's background driver future without awaiting it.
- `set_show_and_reset_are_accepted_and_leave_the_connection_usable` -
  `SET extra_float_digits = 3` and `RESET extra_float_digits` both
  complete without reaching `engine::Database` at all; `SHOW
  server_version` returns a one-row, one-column result naming the
  parameter and carrying a non-empty value from the fixed parameter table
  `wire.rs`'s `intercept_set_show_reset` reuses from the startup
  handshake; `SHOW nonexistent_parameter` succeeds with an empty string
  rather than an error; and the connection still runs an ordinary
  `CREATE TABLE`/`SELECT` afterward.
- `set_show_and_reset_keywords_are_matched_case_insensitively` - lowercase
  `set` and mixed-case `Reset` are both recognized, since only the leading
  keyword's case is folded.

## Usage Example

```rust
let db = Arc::new(Database::open(DbConfig::new(db_path)).unwrap());
let client = connect(db).await;

client.simple_query("SET extra_float_digits = 3").await.unwrap();
let show = client.simple_query("SHOW server_version").await.unwrap();
assert!(matches!(show[0], SimpleQueryMessage::Row(_)));
```

Run via `cargo test -p server --test wire_set_show_reset`. Together with
`wire_simple_query.rs` and `wire_errors.rs`, this is the automated proof
that M13.2's wire listener accepts the handshake noise pgjdbc and other
clients send before a query is ever considered a statement, without
touching `sql`'s grammar.
