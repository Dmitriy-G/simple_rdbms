# logging

Proves two of CLAUDE.md's logging-section promises hold against
`Database::execute`'s real output, by installing a real `tracing`
subscriber that captures its JSON records instead of discarding them and
parsing them back out with `serde_json` - the same technique
`storage/tests/recovery_logging.rs` uses for `recovery::recover`'s summary
event, applied here to the connection/transaction/statement span
hierarchy and the literal-redaction rule.

## Key Components

- `CaptureBuf` - a `Clone`able `Write` implementation backed by a shared
  `Arc<Mutex<Vec<u8>>>`, so a `tracing_subscriber::fmt` layer's writer
  closure can hand out clones that all append to the same buffer. Its
  `Write` impl and `captured_events` below both carry `#[cfg(test)]` so
  `clippy.toml`'s `allow-unwrap-in-tests`/`allow-expect-in-tests` apply to
  them - see `storage/tests/recovery_logging.MD` for why a free function
  in a `tests/` file needs the attribute spelled out explicitly rather
  than inheriting it from the file only ever compiling as a test binary.
- `captured_events(buf) -> Vec<serde_json::Value>` - splits the captured
  bytes into lines and parses each non-empty one as a JSON event.
- `set_capturing_subscriber(capture) -> tracing::subscriber::DefaultGuard`
  - installs a JSON `fmt` subscriber at `TRACE` (so even `fetch_page`'s
  hot-path event is captured) writing into `capture`, as the default for
  the current thread; dropping the returned guard restores whatever was
  there before.
- `a_successful_statement_is_logged_at_info_with_no_literal_values` - runs
  an `INSERT` carrying both a numeric and a string literal, finds the
  `info`-level "statement executed" event, and asserts its `fingerprint`
  field contains neither literal's text (only `?` where they were) -
  CLAUDE.md's "never log user data above `DEBUG`" rule, checked against
  real output rather than only against `sql::fingerprint` in isolation
  (`sql/tests/fingerprint_tests.rs` already covers that function alone).
- `a_failed_statement_is_logged_with_fingerprint_and_no_literal_values` -
  runs an `INSERT` whose literal is out of range for its column, finds the
  `warn`-level "statement failed" event, and asserts its `error` field
  (`common::Error::redacted`'s output) still names the column but replaces
  the literal with `?`, and that its `fingerprint` field is equally clean;
  then finds the paired `debug`-level "statement failed" event and asserts
  *its* `err` field does contain the literal - the detail CLAUDE.md's
  logging section promises is still available one level down for an
  operator who turns `RUST_LOG` up deliberately.
- `the_transaction_spans_txn_id_reaches_an_event_logged_deep_in_storage` -
  runs `BEGIN`/`INSERT`/`COMMIT`, then finds the `storage::buffer::fetch_page`
  `trace!` events the `INSERT` triggered several crates below `engine`,
  and asserts at least one has the transaction span's `txn_id` in its
  ancestor `spans` list. `storage` never receives a `txn_id` as a
  parameter anywhere on this path - the only way it could appear on that
  event is the span hierarchy `database.MD` describes actually nesting a
  transaction span around the statement that joins it.

## Usage Example

```sh
cargo test -p engine --test logging
```

Run as part of `cargo test --workspace`.
