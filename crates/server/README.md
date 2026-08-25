# server

The headless counterpart to `cli`: opens a database, serves Prometheus
metrics and liveness/readiness over HTTP, and shuts down gracefully on
`SIGTERM`/`Ctrl-C` — built for a container, not a human at a terminal.

## Architecture

`server` sits at the same layer as `cli` — it may depend only on `engine`
and `common`, per the workspace's dependency-edge rules (see
`docs/adr/0002-crate-splitting.md` and CLAUDE.md) — but is a separate
binary and a separate crate rather than a mode of `cli`, because the two
have genuinely different main loops: `cli`'s reads statements from stdin
for an interactive session; `server`'s binds a listener, opens the
database, and blocks waiting for a shutdown signal. Forcing both into one
binary would mean those two loops fighting over what "the main loop"
even means.

There is no SQL wire protocol yet (that's `docs/ROADMAP.md`'s M14, which
this crate is an explicit skeleton for) — today `server` opens a
`Database` purely so its metrics reflect a real, running engine and its
readiness check reflects a real, completed recovery, not because anything
external can submit it a statement yet.

No async runtime: this workspace has none anywhere (see the root
`README.md`), and `metrics-exporter-prometheus`'s own built-in HTTP
listener needs one (its `http-listener` Cargo feature pulls in `tokio`).
Pulling in an entire async runtime for three trivial routes would be a
far larger architectural change than the routes themselves justify, so
this crate uses the exporter in manual-render mode
(`PrometheusBuilder::install_recorder`, `default-features = false` on the
dependency) and serves the result itself with a small, synchronous
`std::net::TcpListener` loop — see `src/http.MD`.

## Key Components

- `main` - argument parsing and startup/shutdown wiring: install the
  Prometheus recorder, spawn the HTTP listener, open the `Database`, mark
  ready, block for a shutdown signal, then checkpoint and close. See
  `src/main.MD`.
- `health` - `Readiness`, the liveness/readiness state shared between
  `main` and the HTTP listener thread. See `src/health.MD`.
- `http` - the hand-rolled synchronous responder for `/metrics`,
  `/health/live`, and `/health/ready`. See `src/http.MD`.
- `signals` - blocks until `SIGTERM`/`Ctrl-C`, so `main` can run a
  graceful shutdown instead of the process just dying mid-write. See
  `src/signals.MD`.

## Features

Metrics (buffer pool hits/misses/evictions/pinned frames, disk reads/
writes, WAL bytes/fsyncs, double-write batches/pages restored, checkpoint
duration/LSN, transactions committed/aborted, recovery duration/losers,
query duration by statement kind — see `docs/ROADMAP.md`'s M13 entry),
liveness, readiness, and graceful `SIGTERM`/`Ctrl-C` shutdown all work
today. What doesn't: anything that lets an external client actually run a
statement against the `Database` this binary opens — no wire protocol, no
other network-facing route. That's M14, deliberately out of scope here.

## Dependencies

Workspace: `common`, `engine`. External: `anyhow`, `clap` (argument
parsing, same as `cli`); `tracing`/`tracing-subscriber` (JSON logging to
stdout — see `src/main.MD` for why stdout here and not `cli`'s stderr);
`metrics` (the facade every instrumented crate below this one already
calls into); `metrics-exporter-prometheus` with `default-features = false`
(no `tokio` — see Architecture); `ctrlc` with its `termination` feature
(catches `SIGTERM` on Unix in addition to `SIGINT`/`Ctrl-C` everywhere).
Dev-only: `tempfile`.

## Configuration

`--metrics-addr` (default `0.0.0.0:9090`) and `--health-check` (a bare
flag; queries this process's own `/health/ready` and exits `0`/`1` - see
`src/main.MD`) are the two flags beyond `cli`'s own `db_path` positional
argument. Every database sizing knob comes from `common::DbConfig`'s
defaults, the same as `cli` - this binary does not yet expose flags for
them. Logging is controlled by `RUST_LOG`, same as `cli` - see
CLAUDE.md's logging section.

### Container image

`Dockerfile` builds this crate's binary with
[`cargo-chef`](https://github.com/LukeMathWalker/cargo-chef) rather than
a hand-rolled stub-crate dependency-caching stage: an eleven-crate
workspace makes per-crate stubs fiddly to keep in sync, where cargo-chef
computes the dependency-only build plan from `Cargo.lock` alone. The
builder (`rust:1.85.0-slim-bookworm`) and runtime (`debian:bookworm-slim`)
images are both pinned to explicit tags rather than the floating `rust:1`/
`debian:stable-slim` aliases the Dockerfile used before, and deliberately
share the `bookworm` Debian release so the runtime's glibc is never older
than what the binary was linked against. `1.85.0` matches this
workspace's `rust-version` (`Cargo.toml`, driven by `edition = "2024"`'s
minimum supported compiler). Bump both together, deliberately, on
purpose - not because either image floated out from under the build.

## Testing

`health.rs` and `http.rs` each carry an inline `#[cfg(test)] mod tests`
(see `src/health.MD` and `src/http.MD`) rather than an integration test
under `tests/`: this crate has only a `[[bin]]` target, no `[lib]`, so a
file under `tests/` cannot `use server::...` at all - there's no library
crate for it to link against. The only external-test option would be
spawning the compiled binary as a subprocess, the way
`crates/cli/tests/crash_recovery.rs` does; that's the right shape for a
black-box, real-process check (see "Verify, don't assume" below), but the
wrong shape for `Readiness`'s state transitions and `/health/ready`'s
per-state response body, which are deterministic in-process behavior an
inline unit test checks directly, without racing a real subprocess's
startup window. Run this crate's tests with:

```sh
cargo test -p server
```

### Verify, don't assume

Unit tests cover `Readiness` and `http::serve` in isolation; they don't
prove the container actually works end to end. After changing
`Dockerfile`/`docker-compose.yml`, build the image and run it for real:

```sh
docker compose build
docker compose up -d
curl -i http://localhost:9090/health/ready   # 503 while recovering, then 200
docker compose stop simple_rdbms             # look for the shutdown log line
docker compose start simple_rdbms            # recovery summary should show 0 losers
docker compose down && docker compose up --build -d   # data must survive
curl http://localhost:9090/health/ready
```

<!-- Transcript of the above, from the pinned bookworm-based images, goes here. -->
