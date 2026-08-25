# endpoints

`server::http::serve`'s `/health/ready` route, exercised end to end: a
real `TcpListener` bound to an ephemeral port, a real `TcpStream` sending
a bare `GET` request line, and the actual response bytes parsed back.
Formerly an inline `#[cfg(test)] mod tests` in `crates/server/src/http.rs`
(already integration-shaped - binding a real listener and making real HTTP
requests - just in the wrong location for it); moved here once `server`
gained a `[lib]` target (see `crates/server/src/lib.MD`). Neither test
touches `http`'s private `read_request_path` directly, so it stays
private - only `serve` and `server::health::Readiness` are needed.

## Key Components

- `spawn_test_server(readiness) -> SocketAddr` - binds `serve` to an
  ephemeral `127.0.0.1` port on its own thread with a fresh, unregistered
  Prometheus recorder handle, and returns the address to connect to.
  Marked `#[cfg(test)]`, as is `get` below, so `clippy.toml`'s
  `allow-expect-in-tests` applies to their `.expect(...)` calls - see
  `storage/tests/recovery_logging.MD` for why a free function in a
  `tests/` file needs the attribute spelled out explicitly.
- `get(addr, path) -> (String, String)` - sends a bare `GET <path>
  HTTP/1.1` request line and returns the response's status line and body,
  split on the blank line that separates HTTP headers from a body.
- `health_ready_reports_each_non_ready_state_as_503_and_names_it` - walks
  `/health/ready` through all three `ReadinessState`s on one running
  server (`starting` -> `set_ready` -> `ready` -> `set_not_ready` ->
  `shutting_down`), asserting each response's status code and body text -
  the two non-ready states must be distinguishable, not just "not 200",
  which is why the body is checked and not only the status.

## Usage Example

```rust
use server::health::Readiness;
use server::http::serve;

let readiness = Readiness::new();
let addr = spawn_test_server(readiness.clone());
let (status, body) = get(addr, "/health/ready");
assert!(status.contains("503"));
assert_eq!(body, "starting\n");
```

Run via `cargo test -p server --test endpoints`, alongside
`tests/readiness.rs`'s state-machine-only check of the same transitions -
this file additionally proves `http::serve` actually wires
`Readiness::state()` into the HTTP response it claims to.
