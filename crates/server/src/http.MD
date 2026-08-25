# http

A hand-rolled, synchronous HTTP responder for exactly three routes:
`/metrics`, `/health/live`, `/health/ready`. Not a general-purpose HTTP
server - it reads one request line and ignores headers and any body,
which every caller this binary actually has (Prometheus's scraper,
Docker's `HEALTHCHECK`, `curl`) satisfies with a bare `GET` line. Written
this way on purpose: `metrics-exporter-prometheus`'s own built-in listener
requires its `http-listener` feature, which pulls in `tokio` - and this
workspace has no async runtime anywhere else (see
`docs/adr/0002-crate-splitting.md` and the root `README.md`). Pulling in
an entire async runtime for three trivial routes would be a much larger
architectural change than the routes themselves justify, so this crate
uses `metrics_exporter_prometheus::PrometheusBuilder::install_recorder`
(which only installs the global recorder and hands back a renderable
`PrometheusHandle` - no listener, no `tokio`) and serves that handle's
output itself with `std::net::TcpListener`.

## Key Components

- `serve(listener, handle, readiness)` - accepts connections from
  `listener` forever, one at a time, handling each before accepting the
  next - acceptable for a metrics/health endpoint scraped occasionally by
  one or two callers, not a design meant to hold up under real concurrent
  load. A failed `accept` is logged at `warn` and does not stop the loop;
  a single bad connection must not take the endpoint down.
- `handle_connection(stream, handle, readiness)` - private. Reads the
  request's path via `read_request_path`, then answers: `/metrics` with
  `handle.render()` (the current Prometheus text exposition), `/health/live`
  with a bare `200`, `/health/ready` with `200` and body `"ready\n"` when
  `readiness.state()` is `ReadinessState::Ready`, or `503` with the
  state's own `Display` name as the body (`"starting\n"` or
  `"shutting_down\n"`) otherwise - see `health.MD` for why those two
  non-ready states are kept distinct rather than collapsed into one
  `503`, anything else with `404`. Writes a minimal `HTTP/1.1` response
  by hand (status line, `Content-Type`, `Content-Length`,
  `Connection: close`, body) - simple enough not to need a crate for it,
  once the request side is this simple too.
- `read_request_path(stream) -> Option<String>` - private. Reads exactly
  one line (the request line, e.g. `GET /metrics HTTP/1.1`) and returns
  its second whitespace-separated field. Returns `None` on any I/O error
  or a malformed line, which `handle_connection` treats as "give up on
  this connection" rather than a `404` - a caller that can't even send a
  valid request line isn't going to parse a response either.

## Usage Example

```rust
use std::net::TcpListener;

use metrics_exporter_prometheus::PrometheusBuilder;

let handle = PrometheusBuilder::new().install_recorder()?;
let listener = TcpListener::bind("127.0.0.1:9090")?;
http::serve(listener, handle, Readiness::new());
```

`main` spawns `serve` on its own thread immediately after binding the
listener, before `Database::open` runs - so `/health/live` (and a
`503` from `/health/ready`) are answerable for the full, possibly
minutes-long duration of crash recovery, not only once the database is
up.
