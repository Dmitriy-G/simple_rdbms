# wire_pg_catalog

End-to-end tests of `server::pg_catalog`'s interception of introspection
queries (`docs/ROADMAP.md`'s M13.4), driving `server::wire::serve` over
`tokio_postgres::Client::simple_query` (the simple query protocol) and,
for the extended-query interception of M13.4 subtask 5,
`Client::prepare`/`Client::query`, against an ephemeral `127.0.0.1:0`
listener and a `tempfile`-backed database, in the same shape as
`wire_simple_query.rs`/`wire_extended_query.rs`.

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
  `pg_catalog::answer` does not recognize - still passes straight through
  to the engine, which now answers it as ordinary SQL (a projection over
  `planner::LogicalPlan::OneRow`) rather than reporting a syntax error.
  The point of the assertion is unchanged: what comes back is the
  engine's own answer, `column1 = 1`, never a value this module invented
  for a shape it did not recognize.
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
- `pg_attribute_lists_columns_for_a_table_filtered_by_oid_in_attnum_order` -
  for a table created as `(flag BOOLEAN, n INTEGER, name TEXT)`, first
  reads the table's oid back from a `pg_class` query (exactly as a real
  driver would), then a `pg_attribute` query filtered by
  `attrelid = <that oid>` returns exactly three rows, in declaration order
  (`flag`/`1`, `n`/`2`, `name`/`3`), each `atttypid` matching
  `tokio_postgres::types::Type::{BOOL,INT4,VARCHAR}.oid()` respectively -
  proving `answer_pg_attribute`'s row order already matches `attnum`
  without a separate sort step, and that `atttypid` agrees with
  `pg_catalog::pg_type_of`'s own mapping.
- `pg_type_reports_int4_and_zero_rows_for_an_unknown_typname` -
  `SELECT oid, typname FROM pg_type WHERE typname = 'int4'` returns one
  row whose `oid` matches `tokio_postgres::types::Type::INT4.oid()`, and
  the same shape with `typname = 'lo'` (a real PostgreSQL type this engine
  has no `DataType` for) returns zero rows rather than an error.
- `an_unrecognized_pg_relation_answers_zero_rows_with_its_own_select_list_columns` -
  `SELECT indexrelid, indisunique FROM pg_index WHERE indrelid = 1`
  answers a `RowDescription` naming exactly `indexrelid`/`indisunique`
  (from the query's own select list, per `select_item_name`) and zero
  rows, never reaching the engine at all; `SELECT * FROM
  pg_catalog.pg_settings` (the `pg_catalog.`-prefixed form, with `*`)
  likewise answers zero rows without erroring.
- `a_query_against_a_real_missing_table_still_fails_as_undefined` -
  `SELECT * FROM nosuchtable` is not `pg_`-prefixed, so
  `answer_unrecognized_pg_relation` never claims it, and it still reaches
  `sql::Parser`/the engine and fails with a real `42P01`, exactly as it
  would if `pg_catalog` did not intercept anything.
- `pg_class_over_the_extended_protocol_sees_a_table_created_after_parse` -
  `client.prepare("SELECT relname FROM pg_class WHERE relname = 't'")`
  succeeds with zero reported parameters *before* table `t` exists (proving
  `StatementParser::parse_sql`/`get_parameter_types`/`get_result_schema`
  never call `Database::describe` for a recognized introspection query),
  then `CREATE TABLE t (...)` runs over `simple_query`, then
  `client.query(&stmt, &[])` on the already-prepared statement returns the
  one row naming `t` - proving `ExtendedQueryHandler::do_query` recomputes
  the `pg_class` answer at `Execute` time rather than reusing whatever it
  saw (nothing) at `Parse` time.
- `a_real_table_named_pg_something_is_read_from_the_engine_not_intercepted` -
  the P-82 regression. `CREATE TABLE pg_foo (a INTEGER)` and one `INSERT`,
  then `SELECT a FROM pg_foo` must return that row: before the fix, the
  `pg_`-prefix catch-all shadowed the real table and answered zero rows
  under a `?column?` heading while the `INSERT` had reported success, which
  is a silent wrong answer rather than an error. The second half of the
  test is the boundary of the fix rather than the fix itself:
  `SELECT a FROM pg_catalog.pg_foo` still answers zero rows, because
  writing the schema out names the catalog explicitly and must not resolve
  to a user's table (`pg_catalog.MD`).
- `a_pg_prefixed_table_created_after_parse_invalidates_the_cached_plan` -
  the extended-protocol half of the same question, and the one case the
  existence check cannot answer with rows. `prepare("SELECT a FROM
  pg_later")` succeeds as an introspection statement while no such table
  exists, so `Describe` has already promised the client a `VARCHAR`
  column; `pg_later` is then created as an `INTEGER` table. `Execute` must
  not answer zero rows (the same silent lie, one protocol over) and must
  not run the real query under the promised types (a driver-level
  deserialization failure), so it returns `0A000` - and re-preparing the
  identical text then reads the real row, which is what proves the error
  is about the cached plan and not about the table.
- `pg_database_names_the_one_database_this_process_serves` - the P-90
  regression, and the reason every assertion here is about row *content*:
  before the fix `pg_database` was claimed by the zero-row catch-all, so a
  query that merely "succeeded" proved nothing at all. `SELECT oid,
  datname, datdba, datallowconn FROM pg_database` answers exactly one row
  whose `datname` is the harness's own file stem
  (`wire_pg_catalog_pg_database`, from `open(dir, name)`), not a constant -
  which is what ties the answer to `Database::database_name()` rather than
  to a fabricated name - with `datallowconn = 't'` and `datdba = '10'`.
  The filters are then exercised both ways: `datname =` the real name
  answers one row, `datname = 'nope'` answers zero, and `oid =` the oid
  the first query reported answers one, proving the fabricated oid is
  stable rather than recomputed per query.
- `pg_roles_and_pg_user_answer_the_one_implicit_superuser` - `pg_roles`
  answers one row, `postgres`, with `rolsuper`/`rolcanlogin` as the
  text-format `t`; `pg_user` answers the same role under its own column
  names, with `usesysid` equal to that role's `oid`. The last assertion is
  the one worth keeping: every `nspowner` `pg_namespace` reports must
  equal that same oid, so a client resolving a schema's owner finds a role
  the server actually lists instead of a dangling oid. It reads both
  namespace rows rather than filtering on `nspname`, because
  `answer_pg_namespace` deliberately honors no `WHERE` clause at all
  (`pg_catalog.MD`). A `rolname` filter naming anyone else answers zero
  rows.
- `pg_database_over_the_extended_protocol_describes_its_columns` -
  `prepare("SELECT datname FROM pg_database")` reports zero parameters and
  `query` returns the one row, proving the new recognizers are reached
  from `StatementParser::parse_sql` and `ExtendedQueryHandler::do_query`
  and not only from the simple query path - the same two-call shape
  `pg_class_over_the_extended_protocol_sees_a_table_created_after_parse`
  checks for `pg_class`.

## Usage Example

```rust
let result = client.simple_query("select version()").await?;
```

Run as part of `cargo test --workspace`; this is the crate's coverage for
`pg_catalog::answer`'s interception in both `SimpleQueryHandler::do_query`
and `ExtendedQueryHandler`/`QueryParser`, complementing
`wire_simple_query.rs`'s and `wire_extended_query.rs`'s coverage of the
two protocols' ordinary paths.
