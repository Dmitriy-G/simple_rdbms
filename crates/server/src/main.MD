# main

`simple_rdbms_server`'s binary entry point: parses command-line
arguments, initializes logging, and runs the startup/shutdown sequence.
Distinct from `cli` on purpose: `cli` reads statements from stdin for an
interactive session, which a container's `HEALTHCHECK`/`docker stop`
lifecycle has no equivalent of, and mixing the two responsibilities into
one binary would mean the REPL's prompt-and-buffer loop and this binary's
listen-and-wait loop fighting over what "the main loop" even means.
Everything this binary drives - `Readiness`, the HTTP responder, the
signal wait - lives in the `server` library (`src/lib.rs`, see
[lib.MD](lib.MD)) rather than here, specifically so it is reachable from
`crates/server/tests/`: a binary-only crate has no library target for an
integration test to link against, so this crate now ships both
(`crates/server/Cargo.toml`'s `[lib]`/`[[bin]]` split).

## Key Components

- `Args` - command-line arguments, parsed by `clap::Parser`: `db_path`
  (positional, defaults to `"simple_rdbms.db"`, same default `cli` uses),
  `--metrics-addr` (defaults to `DEFAULT_METRICS_ADDR` - the port
  `docker-compose.yml` publishes, deliberately separate from the SQL port
  so an operator can scrape metrics without going through the database
  protocol at all; also readable from the `SIMPLE_RDBMS_METRICS_ADDR`
  environment variable via clap's `env`, so a `docker-compose.yml` override
  of the listener address and the `HEALTHCHECK`'s own `--health-check`
  probe of it move together instead of the healthcheck silently querying
  the default port forever - see `docker-compose.yml`), `--pg-addr`
  (defaults to `DEFAULT_PG_ADDR`, also readable from
  `SIMPLE_RDBMS_PG_ADDR`, same shape as `--metrics-addr` - the address
  `server::wire::serve` listens on), and `--health-check` (a bare flag;
  see `check_ready` below).
- `DEFAULT_PG_ADDR` - private. `"127.0.0.1:5432"`: the SQL port carries no
  authentication of any kind until M22 (`docs/ROADMAP.md`), so anything
  that can reach it can run arbitrary SQL, and the default therefore keeps
  it on loopback. `docs/ROADMAP.md`'s M13.2 entry records this as the
  constraint M22 lifts.
- `DEFAULT_METRICS_ADDR` - private. `"127.0.0.1:9090"`, for the weaker
  version of the same reason: `/metrics`, `/health/live` and
  `/health/ready` (see [http.MD](http.MD)) need no credential either, and
  while no metric in this workspace carries user data - every one is an
  engine-internal counter, gauge or histogram, and the only label anywhere
  is `statement_kind` - they do describe the workload and the durability
  sizing of a running database, which is not something to serve to every
  interface a host happens to have.
- **The binary defaults to loopback; the image overrides it.** Both
  constants are deliberately unsafe-by-omission rather than
  unsafe-by-default: binding anything else is an operator decision, made
  by passing `--metrics-addr`/`--pg-addr` or setting the two environment
  variables. `Dockerfile` makes exactly that decision, with `ENV
  SIMPLE_RDBMS_METRICS_ADDR=0.0.0.0:9090` and `ENV
  SIMPLE_RDBMS_PG_ADDR=0.0.0.0:5432` beside its `EXPOSE` lines, because
  inside a container binding `0.0.0.0` is not an exposure decision at all
  - the namespace belongs to that container, Docker forwards a published
  port to its interface and never to its loopback, and what exposes the
  port is the operator publishing it. `docker-compose.yml` then publishes
  both on the host's loopback. Two unit tests in `main.rs` assert each
  constant parses to a loopback address, so widening either default fails
  the suite rather than passing quietly.
- `main() -> anyhow::Result<()>` - parses `Args` first, before
  `init_logging`, because `--health-check` short-circuits everything else:
  if set, `main` calls `check_ready` against this process's own
  `--metrics-addr` port and exits `0` or `1` without opening a database,
  installing a recorder, or logging anything - it is a one-shot client
  query against the *other*, long-running invocation of this same binary
  that `Dockerfile`'s `HEALTHCHECK` actually manages the container with,
  not a mode this invocation's own server needs. Otherwise, `init_logging`,
  then, in order: install the global Prometheus recorder
  (`PrometheusBuilder::install_recorder`),
  bind the metrics/health `TcpListener` and spawn `http::serve` on its own
  thread *before* opening the database, so liveness and a `503` readiness
  are answerable through the entire recovery window; open the `Database`,
  wrapped in an `Arc` (this is where crash recovery runs); call
  `Readiness::set_ready`; build a multi-thread `tokio` `Runtime`, bind
  `--pg-addr` on it, and spawn `server::wire::serve` on that runtime with
  a clone of the `Arc<Database>` - the runtime is built only after the
  database is open and ready, so a slow recovery never leaves the SQL
  port accepting connections a `block_in_place` call would have nowhere
  to run; then `signals::wait_for_shutdown_signal`, which blocks until
  `SIGTERM`/`Ctrl-C`. On return from that block, the *first* thing that
  happens is `Readiness::set_not_ready` - before the "shutdown signal
  received" log line, before the `tokio` runtime is shut down - so
  `/health/ready` starts returning `503 shutting_down` for the entire
  checkpoint-and-close window instead of claiming ready right up until the
  process disappears. `pg_runtime.shutdown_timeout` then stops the `pgwire`
  listener and every connection task, each of which was holding its own
  clone of the `Arc<Database>`; once that returns, `main` reclaims the
  database with `Arc::try_unwrap` and calls `Database::close` on it before
  the process exits, since `close(self)` takes ownership and an `Arc`
  cannot give it up while any clone is still alive. If a connection's
  `block_in_place` call was still running when the shutdown timeout
  elapsed, `try_unwrap` fails; `main` logs a warning and lets the `Arc`
  drop instead, which still flushes via `Database`'s own `Drop` impl - a
  best-effort fallback rather than the ordinary checkpoint-and-close path.
  The HTTP listener thread's `JoinHandle` is kept (as `_http_handle`, to
  keep it alive without joining it - joining would block forever, since
  `http::serve` only returns by exiting its accept loop) but never
  joined: `http::serve` is wrapped in `std::panic::catch_unwind`, and
  whether the inner call panics or simply returns, the wrapping closure
  logs `tracing::error!` before the thread ends. Without that, a panic in
  the listener would take metrics and health down with no log line at
  all, and the container would be killed by its own `HEALTHCHECK` with no
  evidence of why. The `pgwire` listener task's `JoinHandle` (`_pg_handle`)
  is kept the same way, for the same reason - it is only ever ended by
  `pg_runtime.shutdown_timeout`, never by returning on its own.
- `init_logging()` - the same shape as `cli::init_logging`, except this
  one writes JSON to **stdout**, not stderr: this binary has no
  interactive output competing for that stream (unlike `cli`'s REPL - see
  `crates/cli/src/main.MD`), so it's exactly the process CLAUDE.md's
  logging section describes: "a future headless server binary writes
  JSON to stdout only."
- `check_ready(metrics_addr) -> bool` - private. Takes just the port off
  `metrics_addr` (the same string `--metrics-addr` was given; the host
  part is ignored - a `HEALTHCHECK` always runs inside the same network
  namespace as the server it's checking, so `127.0.0.1` is always the
  right destination regardless of what interface the server bound to),
  connects to `127.0.0.1:<port>`, sends a bare `GET /health/ready
  HTTP/1.1`, and returns whether the status line contains `200` - the
  same pass/fail rule `curl --fail` used to apply. On any I/O failure or a
  non-`200` status it prints one line to stderr (Docker surfaces a
  `HEALTHCHECK`'s stdout/stderr in `docker inspect`'s health log) and
  returns `false`; it never panics, since a panic here would make
  `--health-check` itself the reason the container gets killed. This
  exists so the runtime image doesn't need `curl` installed just to
  satisfy `HEALTHCHECK` - one less package to patch, and the exec-form
  `HEALTHCHECK CMD ["simple_rdbms_server", "--health-check"]` it enables
  never touches a shell, unlike a `curl ... || exit 1` string form would.

## Usage Example

```sh
simple_rdbms_server /data/simple_rdbms.db
curl http://localhost:9090/health/ready
curl http://localhost:9090/metrics
psql -h 127.0.0.1 -p 5432
simple_rdbms_server /data/simple_rdbms.db --pg-addr 0.0.0.0:5432
simple_rdbms_server --health-check; echo "exit code: $?"
```

The fifth line is the opt-in: with no `--pg-addr` (and no
`--metrics-addr`) both ports answer only on loopback, and reaching either
from another host is something an operator asks for by name, knowing there
is no authentication behind it. The container image makes that request
once, in its own `ENV`, for its own namespace.

`Dockerfile`'s `HEALTHCHECK` runs `simple_rdbms_server --health-check`
against the same container's own `/health/ready`; `docker-compose.yml`
publishes `--metrics-addr`'s and `--pg-addr`'s ports on the host's
loopback (`127.0.0.1:9090:9090` and `127.0.0.1:5432:5432`) and mounts the
database file's directory as a `VOLUME` so `docker rm` doesn't take the
data with it. Both container-side addresses are already `0.0.0.0` from the
image's own `ENV`, so `docker run -p 5432:5432 simple_rdbms_server` works
with no `-e` flags at all; `docker-compose.yml`'s `environment:` block
re-states them as overridable defaults (`${SIMPLE_RDBMS_PG_ADDR:-...}`)
rather than as the thing that makes the container reachable.
