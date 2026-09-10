# wire_extended_query

End-to-end tests of `server::wire`'s `ExtendedQueryHandler` implementation
(M13.3), driving `server::wire::serve` over `tokio_postgres` - which
always speaks the extended query protocol for `prepare`/`query`/`execute`
- against an ephemeral `127.0.0.1:0` listener and a `tempfile`-backed
database, in the same shape as `wire_simple_query.rs`.

## Key Components

- `connect(db) -> Result<tokio_postgres::Client, ...>` - private. Identical
  to `wire_simple_query.rs`'s helper of the same name: binds an ephemeral
  listener, spawns `server::wire::serve` on it, and connects a
  `tokio_postgres::Client`.
- `open(dir, name) -> tokio_postgres::Client` - private. Opens a
  `tempfile`-backed `Database` at `dir.join(name)` and connects to it via
  `connect`, folding the two steps every test here needs into one call.
  Marked `#[cfg(test)]` so `clippy.toml`'s `allow-expect-in-tests` applies
  to its `.expect(...)` calls, the same reason `tests/integration.MD`'s
  own `open` helper carries the attribute.
- `row_count(messages) -> usize` - private. Counts the `SimpleQueryMessage::Row`
  entries in a `simple_query` response, for tests that only need to know
  how many rows came back.
- `prepared_select_binds_i32_i64_f64_str_and_bool_parameters` - a `SELECT`
  comparing five columns (`BOOLEAN`, `INTEGER`, `BIGINT`, `DOUBLE`, `TEXT`)
  against five placeholders, prepared once and queried with `bool`/`i32`/
  `i64`/`f64`/`&str` Rust arguments, returns the one row that matches -
  proving `planner::infer_parameter_types` gave each placeholder the right
  column type and `decode_param` decoded each one back correctly.
- `a_null_parameter_stores_a_null` - a prepared `INSERT` bound with
  `Option::<&str>::None` stores a real SQL `NULL`, read back as `None`
  from `SimpleQueryRow::get`.
- `a_prepared_statement_runs_twice_with_different_values` - one `prepare`d
  `INSERT`, `execute`d twice with different `i32` values, produces two rows
  in insertion order - proving a `StoredStatement` is reusable across
  multiple `Bind`/`Execute` cycles.
- `begin_and_commit_wrap_a_prepared_insert` - `BEGIN` and `COMMIT` (over
  `simple_query`) around a prepared, parameterized `INSERT` (over the
  extended protocol) commit that row, proving the two protocols share one
  session and one transaction exactly as `wire.MD` documents.
- `a_wrong_typed_parameter_is_reported_as_an_error_not_a_dropped_connection` -
  `client.prepare_typed` declares a placeholder `VARCHAR` that the server
  itself infers as `Integer` from `WHERE n = $1`; since `do_query` decodes
  using the server's own inferred type regardless of what the client
  declared, binding a non-numeric string fails to decode as an `Integer`
  and comes back as a `PgWireError` carrying `08P01` - a real error the
  client can read (`DbError::code`), not a severed connection - and the
  same connection still answers an ordinary `simple_query` afterward.
- `a_portal_suspends_and_resumes_across_fetch_limited_executes` - inserts
  five rows, then over a `tokio_postgres::Transaction` binds a portal on
  `SELECT a FROM t` and calls `query_portal(&portal, 2)` three times,
  asserting 2, 2 and 1 rows back and that the five values seen across all
  three calls are the inserted rows in order with none repeated or
  skipped - proving `ExtendedQueryHandler::do_query` runs the statement
  exactly once (on the first `Execute`, while the portal is still
  `PortalExecutionState::Initial`) and every later `Execute` against the
  same portal resumes the same `QueryResponse` via `pgwire`'s own
  `Portal::fetch` instead of re-running the query.
- `a_fetch_limit_larger_than_the_row_count_completes_in_one_execute` - the
  same five-row table and portal, but `query_portal(&portal, 10)` in a
  single call returns all five rows - `max_rows` exceeding the remaining
  row count never triggers `PortalSuspended`, since `Portal::fetch`'s
  underlying stream runs out before the requested limit does.

## Usage Example

```rust
let stmt = client.prepare("SELECT a FROM t WHERE a = $1").await?;
let rows = client.query(&stmt, &[&1i32]).await?;
```

Run as part of `cargo test --workspace`; this is the crate's coverage for
`ExtendedQueryHandler`/`QueryParser`, complementing `wire_simple_query.rs`'s
coverage of the simple query protocol and `wire_errors.rs`'s coverage of
SQLSTATE/transaction-status reporting.
