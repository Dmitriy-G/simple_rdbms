# wire_logging

Asserts that a query the wire layer answers **without calling the
engine** is logged, which is the one thing the rest of this crate's tests
cannot see. `engine::runtime` logs one line per statement it executes, so
until P-93 an intercepted query - a `pg_catalog` shape, the
`pg_`-relation catch-all, or `SET`/`SHOW`/`RESET` - left no record at any
level, and a client that broke on one of those answers produced a log
that simply stopped. Logging is the behaviour under test here, which is
the case `CLAUDE.md`'s testing rules allow asserting on log output for -
the same exception `crates/engine/tests/logging.rs` relies on.

This file contains **exactly one `#[test]`**, deliberately. The events it
needs are emitted on `tokio` worker threads that the test never created,
so a thread-local capture would miss them and
`test_support::set_global_capturing_subscriber` has to be used instead; a
global subscriber can be installed only once per process, so a second
test in this binary would panic (`test-support/src/logging.MD`).

## Key Components

- `connect(db) -> tokio_postgres::Client` - private. Binds an ephemeral
  `127.0.0.1:0` listener, spawns `server::wire::serve` on it and connects
  a client, the same shape `wire_pg_catalog.rs` uses. Marked
  `#[cfg(test)]` so `clippy.toml`'s `allow-expect-in-tests` covers its
  `.expect(...)` calls, exactly as `wire_pg_catalog.rs`'s own `open`
  helper is.
- `answered_events(capture) -> Vec<serde_json::Value>` - private. The
  captured JSON events filtered down to `INFO`-level
  `pgwire: answered without the engine` records, so each assertion below
  reads as a statement about one answered query rather than about the
  whole log.
- `every_query_the_wire_layer_answers_itself_is_logged_with_its_shape_and_row_count` -
  drives one recognized `pg_catalog` query (`pg_class`, one row), one
  unrecognized `pg_`-relation query (`pg_index`, zero rows), one `SHOW`,
  and one ordinary `SELECT` that must reach the engine. It asserts that
  each intercepted answer logged its `shape` and its `rows` count, that
  the `peer` field is present so the line joins up with the connection's
  own accept log, and that **no `INFO` event carries a `sql` field** -
  the raw query text is a `debug`-level field, per `CLAUDE.md`'s rule
  that user data never appears above `DEBUG`. The zero-row assertion is
  the one that matters most: a catch-all answer of zero rows is what a
  client turns into a null, and making that visible in one line is why
  this logging exists (P-94).

## Usage Example

```rust
let capture = CaptureBuf::default();
set_global_capturing_subscriber(&capture);
client.simple_query("SELECT relname FROM pg_class").await?;
let events = answered_events(&capture);
assert_eq!(events[0]["fields"]["shape"], "pg_class");
```

Run as part of `cargo test --workspace`. It complements
`wire_pg_catalog.rs`, which checks *what* the interception answers, by
checking that the fact it answered at all is observable to whoever is
reading the log while a real client misbehaves.
