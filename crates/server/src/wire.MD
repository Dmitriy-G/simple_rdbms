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

This subtask (M13.2 subtask 1) wires up only the startup handshake:
`AuthenticationOk`, the fixed `ParameterStatus` set `NoopStartupHandler`'s
default `DefaultServerParameterProvider` already sends (`server_version`,
`client_encoding`, `DateStyle`, `TimeZone`, `integer_datetimes`,
`standard_conforming_strings`, and a few more), `BackendKeyData`, and
`ReadyForQuery`. `do_query` is a placeholder that answers every query
with `Response::EmptyQuery` regardless of its text; real `RowDescription`/
`DataRow`/`CommandComplete` handling over this same session is M13.2's
subtask 3.

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
  default no-authentication handshake is exactly what this subtask
  wants) and `SimpleQueryHandler::do_query` (the `Response::EmptyQuery`
  placeholder above).
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
