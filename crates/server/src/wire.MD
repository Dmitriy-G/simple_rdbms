# wire

The PostgreSQL wire protocol listener (`docs/adr/0007-postgres-wire-protocol.md`,
`docs/ROADMAP.md`'s M13.2): the `pgwire` crate handles wire framing and the
startup/simple-query message flow, and this module supplies the two
handler traits `pgwire` needs to drive that flow against this engine -
`pgwire::api::auth::StartupHandler` (via the no-authentication
`NoopStartupHandler` blanket impl; real authentication is M22) and
`pgwire::api::query::SimpleQueryHandler`. `sql`'s own hand-written parser
is untouched: `pgwire` never sees SQL grammar, only the wire messages that
carry a query string to and a result set back from this module.

Each accepted TCP connection gets its own `engine::Database` session, via
`Database::connect()` (`crates/engine/src/database.rs:47`) on the shared
`Arc<Database>` `main` hands to `serve`. A session is not `Clone` and its
`execute` takes `&mut self`, but the `pgwire` handler traits this module
implements are called through `&self` - the same shared handler instance
answers every message on a connection - so the session lives behind a
`Mutex` inside `ConnectionState`, one `ConnectionState` per connection
rather than one shared across connections the way `pgwire`'s own examples
share a single handler. `PgWireServerHandlers::simple_query_handler` and
`::startup_handler` both hand out clones of the same `Arc<ConnectionState>`
so `pgwire` sees one logical handler with two faces, per the shape
`pgwire`'s own `examples/server.rs` uses for a shared handler.

`Database::connect` and `Database::execute` are both blocking calls - they
wait on the engine's own worker-pool thread over an `mpsc` channel
(`crates/engine/src/runtime.MD`) - so both must run through
`tokio::task::block_in_place` rather than block an async worker thread
directly; the listener is only usable on the multi-thread `tokio` runtime
`block_in_place` requires, which is what `main` builds it as.

This subtask (M13.2 subtask 1) wired up the startup handshake:
`AuthenticationOk`, the fixed `ParameterStatus` set `NoopStartupHandler`'s
default `DefaultServerParameterProvider` already sends (`server_version`,
`client_encoding`, `DateStyle`, `TimeZone`, `integer_datetimes`,
`standard_conforming_strings`, and a few more), `BackendKeyData`, and
`ReadyForQuery`.

M13.2 subtask 3 fills in `do_query`: every query string runs through this
connection's session exactly like `cli` or a `#[test]` would call
`Database::execute`, and the resulting `engine::ResultSet` becomes
`RowDescription`/`DataRow`/`CommandComplete` (for `Rows`) or a bare
`CommandComplete` (for `RowsAffected`/`RolledBack`).
`engine::ResultSet` does not say which statement produced it
(`crates/engine/src/result_set.rs`), so `statement_keyword` reads the
query's own leading token(s) - `"CREATE"` looks one word further, so
`CREATE TABLE` and `CREATE INDEX` are told apart from a bare `CREATE` -
and `to_response` uses that keyword together with the `ResultSet` variant
to pick a tag: `INSERT 0 <n>` for `RowsAffected` after an `INSERT`, the
keyword itself verbatim for every other `RowsAffected` (`BEGIN`, `COMMIT`,
`CREATE TABLE`, `CREATE INDEX`), `ROLLBACK` for `ResultSet::RolledBack`,
and `SELECT`/`EXPLAIN` for `Rows` depending on whether the leading keyword
was `EXPLAIN`. For `Rows`, `pgwire`'s own `send_query_response`
(`pgwire::api::query`) always appends the number of `DataRow`s actually
streamed to whatever command tag `QueryResponse::set_command_tag` was
given, which is exactly right for `SELECT <n>` but means an `EXPLAIN`'s
`CommandComplete` also carries a row count - unlike real PostgreSQL, whose
`EXPLAIN` tag has no count at all. Matching that exactly would mean
bypassing `pgwire`'s default `on_query` and rebuilding its message
sequence by hand; this module accepts the extra number as a known,
harmless deviation instead.

Column types ride on `engine::ResultSet::Rows`'s `column_types`
(`crates/engine/src/result_set.rs`, plumbed from the binder in M13.2
subtask 2) and `field_info` maps each to the fixed PostgreSQL OID the
task called for: `Boolean` to `Type::BOOL` (16), `Integer` to `Type::INT4`
(23), `BigInt` to `Type::INT8` (20), `Double` to `Type::FLOAT8` (701), and
`Varchar(_)`/`None` to `Type::VARCHAR` (25) - `None` (a projection with no
static type, e.g. a bare `NULL` literal) gets the same OID a real `NULL`
literal column gets in PostgreSQL itself. Every field is always reported
in **text** format (`FieldFormat::Text`): no type modifier is plumbed
through, so a `VARCHAR`'s declared length is invisible on the wire - only
its OID says "this is text". `encode_data_row` builds each `DataRow`'s
bytes directly (length-prefixed UTF-8, or `-1` for a SQL `NULL`) rather
than going through `pgwire`'s `DataRowEncoder`, since that encoder wants
values that implement `postgres_types::ToSql`/`ToSqlText` and
`types::Value` implements neither. `encode_value_text` is the text
encoding for each `types::Value`: `Boolean` as `t`/`f` (PostgreSQL's own
text booleans, not `true`/`false`), `Integer`/`BigInt`/`Double` through
their `Display` impl, `Varchar` verbatim, and `Null` as `None` - which
`encode_data_row` turns into the protocol's `-1`-length null marker
rather than an empty string.

A simple query carries exactly one statement:
`sql::Parser::parse` (`crates/sql/src/parser.rs`) accepts one optional
trailing semicolon and rejects anything after it, so a multi-statement
simple query (`SELECT 1; SELECT 2;`) surfaces to the client as a syntax
error rather than running both statements. This is a known limitation,
also called out in `crates/server/README.md`.

Any `common::Error` `Database::execute` returns is still wrapped in a
generic `PgWireError::UserError` with SQLSTATE `XX000` here - the real
per-error SQLSTATE, severity and `ReadyForQuery` transaction status are
M13.2 subtask 4's job, not this one's.

## Key Components

- `serve(listener, db) -> impl Future<Output = ()>` - the listener's main
  loop. Accepts connections forever; a failed `accept` is logged and does
  not stop the loop. Each accepted socket is handed to its own spawned
  `tokio` task, which opens a session (`Database::connect`, via
  `block_in_place`), builds this connection's `ConnectionHandlers`, and
  runs `pgwire::tokio::process_socket` on it. A session-open failure or
  an I/O error partway through a connection's protocol messages is
  logged and only ends that one connection, never the listener.
- `ConnectionState` - private. Holds this connection's session
  (`Mutex<engine::Database>`) and implements the two `pgwire` traits that
  do the actual protocol work: `NoopStartupHandler` (no overrides - the
  default no-authentication handshake is exactly what subtask 1 wants)
  and `SimpleQueryHandler::do_query`, which runs the query string through
  the session (via `block_in_place`) and turns its `Result<ResultSet>`
  into a `Vec<Response>` with `to_response`, or a generic
  `PgWireError::UserError` on failure.
- `to_response(query, result_set) -> Response` - private. Builds the
  `pgwire::api::results::Response` a `ResultSet` becomes: `Response::Query`
  (`RowDescription` + one `DataRow` per row, via `field_info` and
  `encode_data_row`) for `Rows`, `Response::Execution` with a tag from
  `statement_keyword` otherwise.
- `statement_keyword(sql) -> String` - private. The query's leading
  keyword, upper-cased; `CREATE` looks one further word ahead so
  `CREATE TABLE`/`CREATE INDEX` read as two-word keywords.
- `field_info(name, data_type) -> FieldInfo` - private. Maps an
  `Option<types::DataType>` to the `FieldInfo` `RowDescription` needs,
  always in text format.
- `encode_data_row(tuple) -> DataRow` - private. Encodes one `types::Tuple`
  as a wire `DataRow`, via `encode_value_text`.
- `encode_value_text(value) -> Option<String>` - private. The text-format
  encoding of one `types::Value`; `None` means SQL `NULL`.
- `ConnectionHandlers` - private. The `pgwire::api::PgWireServerHandlers`
  a connection is served with; both of its methods return a clone of the
  same `Arc<ConnectionState>`, so `startup_handler` and
  `simple_query_handler` are backed by the one session.

## Usage Example

```rust
use std::sync::Arc;

use engine::Database;
use tokio::net::TcpListener;

async fn run(db: Arc<Database>) -> std::io::Result<()> {
    let listener = TcpListener::bind("127.0.0.1:5432").await?;
    server::wire::serve(listener, db).await;
    Ok(())
}
```

`crates/server/src/main.rs` builds a multi-thread `tokio` runtime after
`Database::open` succeeds, binds the configured `--pg-addr` listener on
it, and spawns `serve` as a task on that runtime; `crates/server/tests/wire_startup.rs`
drives the same function against an ephemeral `127.0.0.1:0` listener and
a `tempfile` database, and proves the handshake completed by connecting
with `tokio_postgres` and observing a successful `connect()`.
`crates/server/tests/wire_simple_query.rs` drives the same listener with
`tokio_postgres::Client::simple_query`, proving `CREATE TABLE`, `INSERT`,
`SELECT`, `BEGIN`/`COMMIT` and `EXPLAIN` all round-trip through this
module's `RowDescription`/`DataRow`/`CommandComplete` handling.
