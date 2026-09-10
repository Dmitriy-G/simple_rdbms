# wire_pg_catalog

End-to-end tests of `server::pg_catalog`'s interception of introspection
queries (`docs/ROADMAP.md`'s M13.4), driving `server::wire::serve` over
`tokio_postgres::Client::simple_query` against an ephemeral
`127.0.0.1:0` listener and a `tempfile`-backed database, in the same shape
as `wire_simple_query.rs`.

## Key Components

- `connect(db) -> Result<tokio_postgres::Client, ...>` - private. Identical
  to `wire_simple_query.rs`'s helper of the same name: binds an ephemeral
  listener, spawns `server::wire::serve` on it, and connects a
  `tokio_postgres::Client`.
- `open(dir, name) -> tokio_postgres::Client` - private. Opens a
  `tempfile`-backed `Database` at `dir.join(name)` and connects to it via
  `connect`, folding the two steps every test here needs into one call.
  Marked `#[cfg(test)]` so `clippy.toml`'s `allow-expect-in-tests` applies
  to its `.expect(...)` calls, the same reason `wire_extended_query.rs`'s
  own `open` helper carries the attribute.
- `only_row(messages) -> &SimpleQueryRow` - private. Asserts a
  `simple_query` response carries exactly one `SimpleQueryMessage::Row`
  and returns it, for tests that expect a single-row answer.
- `select_version_lowercase_answers_a_postgresql_version_string` -
  `select version()` answers one row whose `version` column starts with
  `PostgreSQL `.
- `select_version_uppercase_with_semicolon_answers_a_postgresql_version_string` -
  the same, but `SELECT VERSION();` - proving `pg_catalog::normalize`'s
  case-folding and trailing-semicolon stripping both work.
- `an_ordinary_select_is_unaffected` - a real table's `SELECT a FROM t`
  still round-trips normally, and a FROM-less `SELECT 1` - a query
  `pg_catalog::answer` does not recognize - still reaches `sql::Parser`
  and fails with its ordinary `42601 syntax_error`, exactly as it would
  without this module in the path at all; a version-like answer is never
  invented for something that wasn't actually recognized.

## Usage Example

```rust
let result = client.simple_query("select version()").await?;
```

Run as part of `cargo test --workspace`; this is the crate's coverage for
`pg_catalog::answer`'s interception in `SimpleQueryHandler::do_query`,
complementing `wire_simple_query.rs`'s coverage of the ordinary simple
query path.
