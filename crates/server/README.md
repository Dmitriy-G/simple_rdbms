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

`server` now speaks the real PostgreSQL wire protocol
(`docs/adr/0007-postgres-wire-protocol.md`, `docs/ROADMAP.md`'s M13.2): a
`pgwire`-backed listener on `--pg-addr` gives every accepted connection
its own `engine::Database` session, and any client that speaks the
protocol — `psql`, `tokio_postgres`, a JDBC driver — can run real SQL
against it. See `src/wire.MD` for the message flow and
`## Testing`/"Verify, don't assume" below for the automated and manual
proof.

This crate does pull in an async runtime — `tokio`, for the `pgwire`
listener above — which the rest of this workspace still has none of
(`metrics-exporter-prometheus`'s own HTTP listener is deliberately kept
on a synchronous `std::net::TcpListener` loop instead, exactly as before;
see below). `main` builds one multi-thread `tokio::runtime::Runtime` after
`Database::open` succeeds, spawns the `pgwire` listener on it, and shuts
that runtime down before the final checkpoint and close — see
`src/main.MD`. `Database::connect`/`Database::execute` are both blocking
calls, so every handler in `src/wire.rs` that calls into the engine does
so through `tokio::task::block_in_place`, never by blocking an async
worker thread directly.

`metrics-exporter-prometheus`'s own built-in HTTP listener needs an async
runtime too (its `http-listener` Cargo feature pulls in `tokio`), but
pulling one in for three trivial routes would have been a far larger
architectural change than the routes themselves justified back when this
crate had no other reason to need one. So this crate still uses the
exporter in manual-render mode (`PrometheusBuilder::install_recorder`,
`default-features = false` on the dependency) and serves the result
itself with the same small, synchronous `std::net::TcpListener` loop —
see `src/http.MD`. The `pgwire` listener's `tokio` runtime and this
loop's own thread are independent of each other by design.

## Key Components

`server` ships both a `[lib]` and a `[[bin]]` target (same shape as
`cli` - see `crates/cli/README.md`), so its logic is reachable from
`tests/` rather than only from an inline `#[cfg(test)]` module.

- `lib` - re-exports `health`, `http`, `signals`, and `wire` as public
  modules. See `src/lib.MD`.
- `main` - argument parsing and startup/shutdown wiring: install the
  Prometheus recorder, spawn the HTTP listener, open the `Database`,
  mark ready, spawn the `pgwire` listener on a `tokio` runtime, block for
  a shutdown signal, then shut that runtime down and checkpoint and close
  the database. See `src/main.MD`.
- `health` - `Readiness`, the liveness/readiness state shared between
  `main` and the HTTP listener thread. See `src/health.MD`.
- `http` - the hand-rolled synchronous responder for `/metrics`,
  `/health/live`, and `/health/ready`. See `src/http.MD`.
- `signals` - blocks until `SIGTERM`/`Ctrl-C`, so `main` can run a
  graceful shutdown instead of the process just dying mid-write. See
  `src/signals.MD`.
- `wire` - the `pgwire`-backed PostgreSQL wire protocol listener: one
  `engine::Database` session per connection, the startup handshake,
  `SELECT`/`INSERT`/`CREATE TABLE`/`CREATE INDEX`/`BEGIN`/`COMMIT`/
  `ROLLBACK`/`EXPLAIN` over the simple query protocol,
  `SET`/`SHOW`/`RESET` accepted before a query ever reaches the engine,
  the extended query protocol (`Parse`/`Bind`/`Describe`/`Execute`/`Sync`,
  M13.3) with `$n` placeholders bound in text or binary format, and a real
  per-error SQLSTATE and transaction status on every reply. See
  `src/wire.MD`.

## Features

Metrics (buffer pool hits/misses/evictions/pinned frames, disk reads/
writes, WAL bytes/fsyncs, double-write batches/pages restored, checkpoint
duration/LSN, transactions committed/aborted, recovery duration/losers,
query duration by statement kind — see `docs/ROADMAP.md`'s M13 entry),
liveness, readiness, and graceful `SIGTERM`/`Ctrl-C` shutdown all work
today, and so does the PostgreSQL wire protocol on `--pg-addr`: any
`pgwire`-speaking client can open a connection, get a real
`AuthenticationOk`/`ParameterStatus`/`ReadyForQuery` handshake (no actual
authentication yet — that's M22), and run `CREATE TABLE`, `INSERT`,
`SELECT`, `CREATE INDEX`, `BEGIN`/`COMMIT`/`ROLLBACK`, `EXPLAIN`, and
`SET`/`SHOW`/`RESET` over the simple query protocol, with a real
per-statement SQLSTATE on error and an accurate transaction status on
every `ReadyForQuery`. The extended query protocol also works (M13.3):
`Parse`/`Bind`/`Describe`/`Execute`/`Sync`, so a client like pgjdbc or
`tokio_postgres` that always prepares its statements — rather than
sending raw SQL text as a simple query — gets a real `ParameterDescription`
and `RowDescription` from `Describe` (typed from `planner::infer_parameter_types`/
`Database::describe`, never by actually running the statement), and
`Execute` binds `$n` placeholders in either text or binary format before
calling `Database::execute_with_params`. See `src/wire.MD` for exactly
what each message carries and its one known limitation (one statement per
simple query message). What doesn't work yet: `DELETE`/`UPDATE`,
multi-table joins, fetch limits/`PortalSuspended` on the extended
protocol, and any authentication at all — see `docs/ROADMAP.md`.

## Dependencies

Workspace: `common`, `engine`. External: `anyhow`, `clap` (argument
parsing, same as `cli`); `tracing`/`tracing-subscriber` (JSON logging to
stdout — see `src/main.MD` for why stdout here and not `cli`'s stderr);
`metrics` (the facade every instrumented crate below this one already
calls into); `metrics-exporter-prometheus` with `default-features = false`
(the HTTP metrics/health listener still needs no `tokio` of its own — see
Architecture); `ctrlc` with its `termination` feature (catches `SIGTERM`
on Unix in addition to `SIGINT`/`Ctrl-C` everywhere); `tokio` (`rt-multi-thread`,
`net`, `macros`, `signal` — the runtime the `pgwire` listener runs on);
`pgwire` (wire framing and the startup/simple-query/extended-query message
flows; see `src/wire.MD`). Dev-only: `tempfile`, `tokio-postgres` (the test
client `tests/wire_*.rs` drive the listener with).

## Configuration

`--metrics-addr` (default `0.0.0.0:9090`, also readable from the
`SIMPLE_RDBMS_METRICS_ADDR` environment variable as a fallback via clap's
`env`), `--pg-addr` (default `0.0.0.0:5432`, same shape via
`SIMPLE_RDBMS_PG_ADDR` - the `pgwire` listener's address), and
`--health-check` (a bare flag; queries this process's own
`/health/ready` and exits `0`/`1` - see `src/main.MD`) are the flags
beyond `cli`'s own `db_path` positional argument. The environment-variable
fallback exists so `docker-compose.yml` can override the listener address
and have the `HEALTHCHECK`'s own `--health-check` invocation (which reads
the same variable) follow automatically - see "Container image" below;
without it, overriding the address only via `command:` would leave the
healthcheck silently probing the wrong port forever. Every database sizing
knob comes from `common::DbConfig`'s defaults, the same as `cli` - this
binary does not yet expose flags for them. Logging is controlled by
`RUST_LOG`, same as `cli` - see CLAUDE.md's logging section.

### Container image

`Dockerfile` builds this crate's binary with
[`cargo-chef`](https://github.com/LukeMathWalker/cargo-chef) rather than
a hand-rolled stub-crate dependency-caching stage: an eleven-crate
workspace makes per-crate stubs fiddly to keep in sync, where cargo-chef
computes the dependency-only build plan from `Cargo.lock` alone. The
builder (`rust:1.90.0-slim-bookworm`) and runtime (`debian:bookworm-slim`)
images are both pinned to explicit tags rather than the floating `rust:1`/
`debian:stable-slim` aliases the Dockerfile used before, and deliberately
share the `bookworm` Debian release so the runtime's glibc is never older
than what the binary was linked against. `1.90.0` matches this
workspace's `rust-version` (`Cargo.toml`) - bumped up from `1.85` in
M13.2 because `pgwire` and one of its own dependencies require rustc
1.89, past `edition = "2024"`'s own minimum. Bump both together,
deliberately, on purpose - not because either image floated out from
under the build.

## Testing

`tests/readiness.rs` checks `Readiness`'s state transitions in isolation,
and `tests/endpoints.rs` binds a real listener and drives `/health/ready`
over real HTTP requests, both through the `server` library's public API
now that the `[lib]` target makes them reachable from `tests/` - formerly
these lived as inline `#[cfg(test)]` modules in `health.rs`/`http.rs`,
back when this crate had only a `[[bin]]` target and nothing under
`tests/` could `use server::...` at all. A `#[cfg(test)]` unit test in
`src/` is reserved for the rare case that needs access to something that
should stay private (see CLAUDE.md's testing section); nothing in `health`
or `http` does.

`tests/wire_startup.rs`, `tests/wire_simple_query.rs`,
`tests/wire_errors.rs`, `tests/wire_set_show_reset.rs` and
`tests/wire_extended_query.rs` drive `server::wire::serve` end to end over
`tokio_postgres` against an ephemeral `127.0.0.1:0` listener and a
`tempfile`-backed `engine::Database`: the startup handshake, `CREATE
TABLE`/`INSERT`/`SELECT`/`BEGIN`/`COMMIT`/`EXPLAIN` over the simple query
protocol, the real SQLSTATE and transaction-status handling on error,
`SET`/`SHOW`/`RESET`, and (M13.3) `Client::prepare`/`prepare_typed`/
`query`/`execute` over the extended protocol - bound `bool`/`i32`/`i64`/
`f64`/`&str`/`NULL` parameters, a statement reused with different values,
a transaction wrapping a prepared `INSERT`, and a parameter whose bytes
don't decode as the server's own inferred type reported as a real error
rather than a dropped connection - respectively - see each file's own
`.MD` for exactly what it asserts. These, together with the HTTP tests above, are
deterministic, in-process checks; none of them proves the container works
end to end - that's what spawning the compiled binary as a subprocess
would be for, the way `crates/cli/tests/crash_recovery.rs` does it, but
this crate has no equivalent automated test today (see "Verify, don't
assume" below for the manual real-container check that fills that gap in
the meantime). Run this crate's tests with:

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

The wire listener needs the same real-container proof: a unit or
integration test only shows `tokio_postgres` round-tripping through it,
never that an actual `psql` binary - a separate implementation of the
client side of the protocol - is happy with what this crate sends. With
the container above already up (`docker compose up -d`), connect from
the host with:

```sh
psql -h 127.0.0.1 -p 5432 -U simple_rdbms -d simple_rdbms
```

This environment had no `psql` installed locally, so the run below used
a throwaway `postgres:16-alpine` client container on the same
`docker compose` network instead - reaching the server by its compose
service name rather than the published loopback port, but the identical
wire protocol either way:

```sh
docker run --rm -i --network simple_rdbms_default postgres:16-alpine \
  psql -e -h simple_rdbms -p 5432 -U simple_rdbms -d simple_rdbms
```

(`-e` echoes each statement before its result, since `psql` only prints
its interactive banner and prompt when standard input is itself a
terminal, which piping the statements below into `docker run -i` does
not provide.) Real transcript, against the image this `Dockerfile` and
`docker-compose.yml` now build:

```
CREATE TABLE t (a INTEGER, b TEXT);
CREATE TABLE
INSERT INTO t VALUES (1, 'ada'), (2, 'bob');
INSERT 0 2
SELECT * FROM t;
 a |  b
---+-----
 1 | ada
 2 | bob
(2 rows)

BEGIN;
BEGIN
INSERT INTO t VALUES (3, 'cy');
INSERT 0 1
COMMIT;
COMMIT
SELECT * FROM t;
 a |  b
---+-----
 1 | ada
 2 | bob
 3 | cy
(3 rows)

\q
```

This run is also what caught a real bug: `wire.rs`'s `statement_keyword`
split the query on whitespace only, so a semicolon-terminated `BEGIN;` -
exactly what `psql` sends and what this crate's own `tokio_postgres`
tests, which never add the semicolon, do not - never matched the
`"BEGIN"` arm, was tagged as a bare `Response::Execution` instead of
`Response::TransactionStart`, and left `ReadyForQuery` reporting `idle`
through an entire transaction. Fixed by trimming a single trailing `;`
before reading the keyword, since `sql::Parser::parse` never lets more
than one reach this code; see `src/wire.MD` and
`tests/wire_errors.rs`'s `a_semicolon_terminated_begin_still_tracks_transaction_status`.
