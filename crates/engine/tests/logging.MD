# logging

Proves two of CLAUDE.md's logging-section promises hold against
`Database::execute`'s real output, by installing a real `tracing`
subscriber that captures its JSON records instead of discarding them and
parsing them back out with `serde_json` - the same technique
`storage/tests/recovery_logging.rs` uses for `recovery::recover`'s summary
event, applied here to the connection/transaction/statement span
hierarchy and the literal-redaction rule. `CaptureBuf`/`captured_events`/
`set_capturing_subscriber` themselves live in the shared `test-support`
crate now (`crates/test-support/src/logging.MD`) - this file is the
legitimate use CLAUDE.md's testing rules carve out for asserting on log
output, since logging itself is exactly the behavior under test here.
`sessions.rs` used the identical capture-and-inspect pattern to observe
checkpoint completion events once; it has since moved to a real counter
(`crates/engine/tests/sessions.MD`), since that was a proxy for a
different behavior, not a test of logging itself.

## Key Components

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
