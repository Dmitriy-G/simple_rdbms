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
- `row_count(messages) -> usize` - private. Counts the
  `SimpleQueryMessage::Row` entries in a `simple_query` response, for
  tests that only need to know how many rows came back.
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
- `pg_class_lists_a_created_table` - after `CREATE TABLE t (a INTEGER)`, a
  `pg_class` query returns exactly one row naming `t`, `relkind = 'r'`,
  and `relnamespace = '2200'` (the fixed `public` oid).
- `pg_class_filtered_by_a_missing_relname_returns_zero_rows` - the same
  table, but `SELECT relname FROM pg_catalog.pg_class WHERE relname =
  'missing'` (using the `pg_catalog.`-prefixed relation name, proving
  `relation_matches` accepts both forms) returns zero rows rather than an
  error or a fabricated one.
- `pg_namespace_lists_public_and_pg_catalog` - `SELECT oid, nspname FROM
  pg_namespace` with no table created at all still answers exactly two
  rows, `public` and `pg_catalog`, since the two fixed namespaces always
  exist regardless of what tables this database has.
- `pg_class_reports_the_same_oid_for_a_table_across_two_queries` - two
  separate `pg_class` queries for the same table's `oid` (in two separate
  `simple_query` round trips) return the same value, proving
  `table_oid`'s hash-based oid is deterministic per name rather than
  computed differently each time.

## Usage Example

```rust
let result = client.simple_query("select version()").await?;
```

Run as part of `cargo test --workspace`; this is the crate's coverage for
`pg_catalog::answer`'s interception in `SimpleQueryHandler::do_query`,
complementing `wire_simple_query.rs`'s coverage of the ordinary simple
query path.
