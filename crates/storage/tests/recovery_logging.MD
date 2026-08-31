# recovery_logging

Proves `recovery::recover`'s `info!` "recovery complete" summary event
(see `recovery.MD`) actually carries the fields CLAUDE.md's logging
section promises, by installing a real `tracing` subscriber that captures
its JSON output instead of discarding it, then parsing that output back
out with `serde_json`. `CaptureBuf`/`captured_events` themselves live in
the shared `test-support` crate now (`crates/test-support/src/logging.MD`);
this file used to carry its own inline copy, `#[cfg(test)]`-gated on both
the `Write` impl and the free function because `clippy.toml`'s
`allow-unwrap-in-tests`/`allow-expect-in-tests` only recognize an
`.unwrap()`/`.expect()` call as "in a test" when it sits inside a
`#[test]` function or a `#[cfg(test)]`-attributed item, not merely inside
a file under `tests/` - a free helper function or trait impl needed the
attribute spelled out explicitly, redundant as that was in a file that
only ever compiled as a test binary. `test-support` sidesteps the whole
question: it disables `unwrap_used`/`expect_used` for itself directly in
its own `Cargo.toml`, so nothing in it needs `#[cfg(test)]` at all.

## Key Components

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
