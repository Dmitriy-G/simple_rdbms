# wire_startup

`server::wire::serve`'s startup handshake, exercised end to end against a
real `tokio` `TcpListener` bound to an ephemeral `127.0.0.1` port and a
real PostgreSQL wire-protocol client (`tokio_postgres`) rather than a
hand-rolled byte-level check of the handshake messages - `tokio_postgres`
itself refuses to consider its `connect()` future resolved until it has
received `ReadyForQuery` (`tokio-postgres`'s `connect_raw.rs`), so a
successful `connect()` is proof that `AuthenticationOk`, every
`ParameterStatus` message, `BackendKeyData` and `ReadyForQuery` all
arrived and parsed as valid wire protocol. Never binds the real
PostgreSQL port `5432` - always `127.0.0.1:0`, letting the OS assign one,
the same pattern `crates/server/tests/endpoints.rs` uses for the HTTP
listener.

## Key Components

- `a_tokio_postgres_client_completes_the_startup_handshake` - opens a
  `tempfile`-backed `Database`, wraps it in an `Arc`, spawns
  `server::wire::serve` on the test's own multi-thread `tokio` runtime
  (`#[tokio::test(flavor = "multi_thread")]` - `serve` calls
  `tokio::task::block_in_place`, which panics on the default
  current-thread test runtime), then drives a real `tokio_postgres::connect`
  against the bound address. The connection's background driver future is
  itself spawned and dropped without being awaited to completion -
  nothing in this test needs to send a query, only to prove the startup
  phase completed.

## Usage Example

```rust
let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
let addr = listener.local_addr().unwrap();
tokio::spawn(server::wire::serve(listener, db));

let (client, connection) =
    tokio_postgres::connect(&format!("host={} port={} user=x", addr.ip(), addr.port()), tokio_postgres::NoTls)
        .await
        .expect("startup handshake completes");
```

Run via `cargo test -p server --test wire_startup`. This is the only
automated proof M13.2's subtask 1 has that a real Postgres wire client can
talk to this engine at all; the manual `psql` transcript in
`crates/server/README.md` (M13.2's subtask 6) is what proves the same
thing against the actual compiled binary and container image.
