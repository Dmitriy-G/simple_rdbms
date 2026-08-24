# signals

Blocks until the process should shut down, so `main` can run a graceful
shutdown sequence (checkpoint, flush, sync, close) instead of the process
just dying mid-write. Docker sends `SIGTERM` and waits ten seconds before
`SIGKILL`; without handling the former, every container restart pays for
a full crash recovery on the next start - correct, since the WAL makes
that safe, but slow and noisy compared to a clean shutdown. The same
handler also catches `Ctrl-C` (`SIGINT`), for free, when running this
binary directly outside a container.

## Key Components

- `wait_for_shutdown_signal() -> anyhow::Result<()>` - installs a signal
  handler via `ctrlc::set_handler` (this crate's `ctrlc` dependency is
  built with the `termination` feature, so on Unix it catches `SIGTERM`
  and `SIGHUP` in addition to `ctrlc`'s default `SIGINT`) that sends on an
  `mpsc` channel, then blocks on that channel's receiver. Returns once a
  signal arrives; the handler itself does no shutdown work - it only
  wakes up the blocked caller, which is what actually runs the shutdown
  sequence with full access to the open `Database`.

## Usage Example

```rust
let db = engine::Database::open(config)?;
signals::wait_for_shutdown_signal()?;
db.close()?;
```

`main` calls this after `Database::open` and `Readiness::set_ready`, as
the last thing it does before an ordinary, successful return - the whole
process lifetime past that point is "block until asked to stop, then
shut down cleanly."
