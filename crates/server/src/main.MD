# main

`simple_rdbms_server`: the headless counterpart to `cli`. Opens a
database, serves Prometheus metrics and liveness/readiness over HTTP,
and shuts down cleanly on `SIGTERM`/`Ctrl-C` - built for a container
(`docs/ROADMAP.md`'s M13), not a human at a terminal. Distinct from `cli`
on purpose: `cli` reads statements from stdin for an interactive session,
which a container's `HEALTHCHECK`/`docker stop` lifecycle has no
equivalent of, and mixing the two responsibilities into one binary would
mean the REPL's prompt-and-buffer loop and this binary's listen-and-wait
loop fighting over what "the main loop" even means.

## Key Components

- `Args` - command-line arguments, parsed by `clap::Parser`: `db_path`
  (positional, defaults to `"simple_rdbms.db"`, same default `cli` uses)
  and `--metrics-addr` (defaults to `"0.0.0.0:9090"` - the port
  `docker-compose.yml` publishes, deliberately separate from any future
  SQL port so an operator can scrape metrics without going through the
  database protocol at all).
- `main() -> anyhow::Result<()>` - `init_logging`, then, in order: install
  the global Prometheus recorder (`PrometheusBuilder::install_recorder`),
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

## Usage Example

```sh
simple_rdbms_server /data/simple_rdbms.db --metrics-addr 0.0.0.0:9090
curl http://localhost:9090/health/ready
curl http://localhost:9090/metrics
```

`Dockerfile`'s `HEALTHCHECK` calls `/health/ready`; `docker-compose.yml`
publishes `--metrics-addr`'s port for a Prometheus scrape target and
mounts the database file's directory as a `VOLUME` so `docker rm` doesn't
take the data with it.
