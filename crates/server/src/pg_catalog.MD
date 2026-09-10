# pg_catalog

Intercepts the introspection queries a real PostgreSQL client library sends
before it will trust a connection - `select version()`, and (later
subtasks of `docs/ROADMAP.md`'s M13.4) queries against `pg_namespace`,
`pg_class`, `pg_attribute` and `pg_type` - and answers them from this
engine's own live catalog rather than letting them reach `sql::Parser`,
which knows nothing about any relation named `pg_*` and would fail every
one of them with `42P01 undefined_table`, killing the client's connection
before its first real statement. `crates/server/src/wire.rs` calls
`answer` at the very front of `SimpleQueryHandler::do_query`, ahead of
`intercept_set_show_reset`, so a recognized introspection query never
reaches `engine::Database::execute` at all; an unrecognized query falls
through unchanged.

This module never fabricates a row for data the engine doesn't have:
where nothing backs an answer, the answer is zero rows (M13.4 subtask 4),
never an invented value. It is deliberately brittle text matching against
a normalized query string, not a real SQL parser - `sql`'s own grammar
has no notion of `pg_catalog` relations at all, and teaching it one is out
of scope here (`docs/ROADMAP.md`'s M24 is where real `pg_catalog` tables,
answered through actual joins, eventually replace this module).

## Key Components

- `Introspection` - the answer to a recognized query, in the same shape
  `wire.rs`'s `to_response` already turns into a `pgwire::api::results::Response`:
  `columns: Vec<(String, pgwire::api::Type)>` (name and wire type, in
  order) and `rows: Vec<Vec<Option<String>>>` (one inner `Vec` per row,
  one cell per column, `None` for SQL `NULL`) - text-format values only,
  since every recognized query here answers with plain scalars, not this
  engine's own `types::Value`.
- `normalize(sql) -> String` - the text canonicalization every recognizer
  matches against: strips `--` line comments and `/* */` block comments
  (recognized only outside a single-quoted string, exactly like real SQL -
  a literal `--` or `/*` inside a string is just data), lowercases every
  character outside a single-quoted string (a literal keeps its own case,
  since `relname = 'MyTable'` must still match `'MyTable'` verbatim later),
  collapses a run of whitespace outside a string to one space, and trims a
  trailing `;` and any leading/trailing whitespace. Whitespace *inside* a
  string literal is left exactly as written - collapsing it would corrupt
  the literal's actual value, which the M13.4 subtask 2/3 filters need
  intact. String-literal tracking here is intentionally naive (a `'` always
  toggles in/out of a literal; there is no escape handling for `''`), which
  is sufficient for every recognized shape this module matches.
- `answer(db, sql) -> Option<Introspection>` - normalizes `sql` and tries
  each recognizer in turn, returning the first match's `Introspection` or
  `None` when nothing recognizes it, in which case the caller runs the
  query through the engine exactly as before. This subtask (M13.4 subtask
  1) recognizes exactly one shape, `select version()`; `db` is unused
  until subtask 2 needs `Database::table_names`/`table_schema` to answer
  from.
- `is_select_version(normalized)` - private, pure. `true` for
  `"select version()"` and the same with a trailing bare or `as`-prefixed
  single-word alias (`"select version() v"`, `"select version() as v"`),
  matching how a real client may write it; `false` for anything else,
  including a second call or extra arguments.
- `version_row()` - private, pure. The one-cell row `answer` returns for
  `select version()`: `PostgreSQL 15.0 (simple_rdbms <CARGO_PKG_VERSION>)`.
  Real clients gate feature detection on the leading `PostgreSQL <major>`
  token, which is why that prefix is fixed text rather than derived from
  anything.

## Usage Example

```rust
use engine::Database;
use server::pg_catalog;

fn maybe_answer(db: &Database, sql: &str) -> bool {
    pg_catalog::answer(db, sql).is_some()
}
```

`crates/server/src/wire.rs`'s `SimpleQueryHandler::do_query` calls
`pg_catalog::answer` first, under the same session lock every other query
path already takes, and turns a `Some(Introspection)` into a
`Response::Query` via its own `introspection_response`/
`encode_introspection_row` helpers - built the same way `to_response`
builds an ordinary `SELECT`'s `Response::Query`, via `FieldInfo` and
`pgwire::api::results::DataRowEncoder`, but keyed off `Introspection`'s
own `(String, Type)` columns rather than `engine::DataType`, since an
introspection answer's columns don't come from this engine's own SQL
type system. `crates/server/tests/wire_pg_catalog.rs` drives this end to
end over `tokio_postgres::Client::simple_query`.
