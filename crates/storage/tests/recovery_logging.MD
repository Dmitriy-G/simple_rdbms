# recovery_logging

Proves `recovery::recover`'s `info!` "recovery complete" summary event
(see `recovery.MD`) actually carries the fields CLAUDE.md's logging
section promises, by installing a real `tracing` subscriber that captures
its JSON output instead of discarding it, then parsing that output back
out with `serde_json`.

## Key Components

- `CaptureBuf` - a `Clone`able `Write` implementation backed by a shared
  `Arc<Mutex<Vec<u8>>>`, so a `tracing_subscriber::fmt` layer's writer
  closure can hand out clones that all append to the same buffer. Its
  `Write` impl carries `#[cfg(test)]`, and `captured_events` below is
  itself marked `#[cfg(test)]`, even though this whole file only ever
  compiles as a test binary: `clippy.toml`'s `allow-unwrap-in-tests`/
  `allow-expect-in-tests` only recognize an `#[unwrap()]`/`.expect()` call
  as "in a test" when it sits inside a `#[test]` function or a
  `#[cfg(test)]`-attributed item, not merely inside a file under
  `tests/` - a free helper function or trait impl needs the attribute
  spelled out explicitly, redundant as that is here, or the lint still
  fires.
- `captured_events(buf) -> Vec<serde_json::Value>` - splits the captured
  bytes into lines and parses each non-empty one as a JSON event.
- `recovery_summary_event_reports_winners_losers_and_record_count` -
  builds a log directly against a `BufferPool` (bypassing `txn`, which
  this crate can't depend on): one transaction with a durable `Commit` but
  no `End` (a "phantom winner" a crash between the two would leave - a
  transaction whose `End` is also durable leaves the Analysis pass's ATT
  entirely, per `recovery.MD`, so it wouldn't be counted here), and one
  transaction with neither (a loser). Installs a capturing subscriber only
  around the *second* session's `recovery::recover` call, then asserts the
  "recovery complete" event's `winners`, `losers`, `records_scanned`, and
  `duration_ms` fields.

## Usage Example

```sh
cargo test -p storage --test recovery_logging
```

Run as part of `cargo test --workspace`. Unlike most tests in this crate,
this one cares about `tracing` output as data, not just about
`recovery::recover`'s return value or the file's resulting bytes -
`tests/recovery.rs` already covers the latter directly.
