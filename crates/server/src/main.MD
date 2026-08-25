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
  `--metrics-addr` (defaults to `"0.0.0.0:9090"` - the port
  `docker-compose.yml` publishes, deliberately separate from any future
  SQL port so an operator can scrape metrics without going through the
  database protocol at all; also readable from the `SIMPLE_RDBMS_METRICS_ADDR`
  environment variable via clap's `env`, so a `docker-compose.yml` override
  of the listener address and the `HEALTHCHECK`'s own `--health-check`
  probe of it move together instead of the healthcheck silently querying
  the default port forever - see `docker-compose.yml`), and
  `--health-check` (a bare flag; see `check_ready` below).
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
  are answerable through the entire recovery window; open the `Database`
  (this is where crash recovery runs); call `Readiness::set_ready`; then
  `signals::wait_for_shutdown_signal`, which blocks until `SIGTERM`/`Ctrl-C`.
  On return from that block, the *first* thing that happens is
  `Readiness::set_not_ready` - before the "shutdown signal received" log
  line, before `Database::close` - so `/health/ready` starts returning
  `503 shutting_down` for the entire checkpoint-and-close window instead
  of claiming ready right up until the process disappears. Then
  checkpoints and closes the database via `Database::close` before the
  process exits - the graceful-shutdown sequence the whole binary exists
  to provide. The HTTP listener thread's `JoinHandle` is kept (as
  `_http_handle`, to keep it alive without joining it - joining would
  block forever, since `http::serve` only returns by exiting its
  accept loop) but never joined: `http::serve` is wrapped in
  `std::panic::catch_unwind`, and whether the inner call panics or simply
  returns, the wrapping closure logs `tracing::error!` before the thread
  ends. Without that, a panic in the listener would take metrics and
  health down with no log line at all, and the container would be killed
  by its own `HEALTHCHECK` with no evidence of why.
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
simple_rdbms_server /data/simple_rdbms.db --metrics-addr 0.0.0.0:9090
curl http://localhost:9090/health/ready
curl http://localhost:9090/metrics
simple_rdbms_server --health-check; echo "exit code: $?"
```

`Dockerfile`'s `HEALTHCHECK` runs `simple_rdbms_server --health-check`
against the same container's own `/health/ready`; `docker-compose.yml`
publishes `--metrics-addr`'s port for a Prometheus scrape target and
mounts the database file's directory as a `VOLUME` so `docker rm` doesn't
take the data with it.
