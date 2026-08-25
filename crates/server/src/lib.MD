# lib

The library half of `server`'s lib/bin split: re-exports `health`, `http`,
and `signals` as public modules so `crates/server/tests/` can reach them.
`main.rs` is left with only argument parsing, the `--health-check`
short-circuit, logging setup, and the startup/shutdown sequence. See
`crates/server/Cargo.toml`'s `[lib]`/`[[bin]]` split and
`crates/server/README.md` for how the two targets relate - the same shape
`crates/cli` uses for the same reason (see `crates/cli/src/lib.MD`): a
binary-only crate has no library target for an integration test to link
against, so `#[cfg(test)]` in `src/` was the only option until this split
existed.

## Key Components

- `health` - `Readiness`, the liveness/readiness state shared between
  `main` and the HTTP listener thread. See [health.MD](health.MD).
- `http` - the hand-rolled synchronous responder for `/metrics`,
  `/health/live`, and `/health/ready`. See [http.MD](http.MD).
- `signals` - blocks until `SIGTERM`/`Ctrl-C`, so `main` can run a
  graceful shutdown instead of the process just dying mid-write. See
  [signals.MD](signals.MD).

## Usage Example

```rust
use server::health::Readiness;
use server::http;

let readiness = Readiness::new();
let listener = std::net::TcpListener::bind("127.0.0.1:9090")?;
http::serve(listener, handle, readiness);
```

`crates/server/src/main.rs` is this library's only in-workspace consumer:
it constructs one `Readiness`, spawns `server::http::serve` on its own
thread, opens the `Database`, and calls `server::signals::wait_for_shutdown_signal`
to block until shutdown. `crates/server/tests/readiness.rs` and
`crates/server/tests/endpoints.rs` exercise `health` and `http`
independently of `main`, through this same public API.
