# http

A hand-rolled, synchronous HTTP responder for exactly three routes:
`/metrics`, `/health/live`, `/health/ready`. Not a general-purpose HTTP
server - it reads one request line and ignores headers and any body,
which every caller this binary actually has (Prometheus's scraper,
Docker's `HEALTHCHECK`, `curl`) satisfies with a bare `GET` line. Written
this way on purpose, though not for the reason it was originally written
for. When this module was added, the argument was that
`metrics-exporter-prometheus`'s own built-in listener needs its
`http-listener` feature, that feature pulls in `tokio`, and the workspace
had no async runtime anywhere - so three trivial routes were not worth
introducing one. M13.2 introduced one regardless, for something else
entirely: `server` now builds a multi-thread `tokio` runtime for the
`pgwire` listener (`main.MD`), and `tokio` is an ordinary dependency of
this crate. The old sentence survived that change and claimed the opposite
until P-83.

**The loop stays synchronous on a different argument, and it is the
stronger one: this endpoint must keep answering when the runtime does
not.** `/health/live` and `/health/ready` are what an orchestrator uses to
decide whether to restart this container, and a runtime wedged by a
blocking call on a worker thread is exactly the failure they exist to
report. Serving them from their own `std::net::TcpListener` thread means
the answer does not depend on the subsystem being asked about - see
`crates/server/README.md`'s Architecture section, which states the same
division from the outside and is the place it belongs rather than being
restated here. So this crate uses
`metrics_exporter_prometheus::PrometheusBuilder::install_recorder`
(which only installs the global recorder and hands back a renderable
`PrometheusHandle` - no listener, no `tokio`) and serves that handle's
output itself.

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
