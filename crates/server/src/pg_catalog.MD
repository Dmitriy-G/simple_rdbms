# pg_catalog

Intercepts the introspection queries a real PostgreSQL client library sends
before it will trust a connection - `select version()`, `pg_namespace`,
`pg_class` (M13.4 subtask 2), and (later subtasks of
`docs/ROADMAP.md`'s M13.4) `pg_attribute` and `pg_type` - and answers them
from this engine's own live catalog rather than letting them reach
`sql::Parser`, which knows nothing about any relation named `pg_*` and
would fail every one of them with `42P01 undefined_table`, killing the
client's connection before its first real statement.
`crates/server/src/wire.rs` calls `answer` at the very front of
`SimpleQueryHandler::do_query`, ahead of `intercept_set_show_reset`, so a
recognized introspection query never reaches `engine::Database::execute`
at all; an unrecognized query falls through unchanged.

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
  each recognizer in turn (`select version()`, then `pg_namespace`, then
  `pg_class`), returning the first match's `Introspection` or `None` when
  nothing recognizes it, in which case the caller runs the query through
  the engine exactly as before.
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
- `recognizes_relation(normalized, relation)` / `relation_after_from(normalized)` /
  `relation_matches(token, relation)` - private, pure. `relation_after_from`
  splits the normalized query on spaces and returns the token immediately
  following a bare `from` token (a table alias, `where`, or nothing
  further is simply ignored); `relation_matches` accepts that token either
  bare (`pg_class`) or `pg_catalog`-prefixed (`pg_catalog.pg_class`), since
  a real client sends both forms; `recognizes_relation` combines the two
  for one recognizer's guard clause. This is the "deliberately brittle
  text matching" the module-level docs above describe: a real join, a
  second `FROM` item, or a relation name glued to a following comma is not
  handled, since no recognized shape here needs one.
- `where_clause(normalized) -> Option<&str>` - private, pure. Everything
  after a literal `" where "` substring, trimmed; `None` when the
  normalized query has no `WHERE` at all. Trailing `ORDER BY`/`LIMIT` text
  is deliberately left attached rather than stripped, since every
  predicate parser below only searches for its own column token as a
  substring and never assumes the clause ends where the predicate does.
- `find_column_token(clause, column) -> Option<usize>` - private, pure.
  Finds `column` as a whole token inside `clause` - bounded on both sides
  by a non-identifier character or a string edge, so `relname` does not
  match inside a longer identifier and `c.relname`/`t.relname` (an
  aliased column reference) still matches - and returns the byte offset
  just past it, or `None` if the token never appears standalone. The
  shared primitive every predicate parser below is built on.
- `eq_string_predicate(clause, column) -> Option<&str>` - private, pure.
  `column`'s token, then optional whitespace, `=`, optional whitespace,
  then a single-quoted literal - returns the literal's contents (still in
  its original case, since `normalize` never folds a string literal's
  case). `None` if `column` never appears as an `= '...'` comparison.
- `eq_number_predicate(clause, column) -> Option<u64>` - private, pure.
  The same shape as `eq_string_predicate` but for an unquoted run of
  ASCII digits (an oid or similar), parsed as `u64`.
- `relkind_in_list(clause) -> Option<Vec<&str>>` - private, pure. Parses
  `relkind in ('r', 'v', ...)` (aliased or not) into the list of quoted
  kind letters inside the parentheses, for the one predicate shape that
  is a list rather than a single equality.
- `fnv1a_32(input) -> u32` / `table_oid(name) -> u32` - private, pure. The
  32-bit FNV-1a hash and the deterministic table oid it produces:
  `16384 + (fnv1a_32(name) % (u32::MAX - 16384))`, so the same table name
  always maps to the same oid across restarts and across separate queries
  in one session, without this engine needing to persist an oid anywhere.
  `16384` is `FirstNormalObjectId`, the same lower bound real PostgreSQL
  reserves for user-created objects, chosen so a computed table oid can
  never collide with the two fixed namespace oids (`11`, `2200`) or any
  future fixed system oid this module hands out.
- `answer_pg_namespace(normalized) -> Option<Introspection>` - private.
  Recognizes a query whose `FROM` names `pg_namespace` (with or without
  the `pg_catalog.` prefix) and answers the two namespaces this engine
  ever puts a table in: `pg_catalog` at oid `11` and `public` at oid
  `2200`, with a fixed placeholder `nspowner` (`10`, real PostgreSQL's own
  bootstrap superuser oid - this engine has no roles yet) and `nspacl`
  always `NULL`. Unlike `answer_pg_class` below, this recognizer does not
  honor a `WHERE` clause at all - always both rows - since nothing in this
  subtask's test shapes needs it filtered and a real client's own
  `nspname`/`oid` predicate on `pg_namespace` is rare compared to on
  `pg_class`.
- `answer_pg_class(db, normalized) -> Option<Introspection>` - private.
  Recognizes a query whose `FROM` names `pg_class` and answers one row per
  `Database::table_names()` entry - `oid` (`table_oid`), `relname`,
  `relnamespace` (always `2200`, `public`), `relkind` (always `r`, an
  ordinary table - this engine has no views/sequences/indexes as
  relations), `relnatts` (`Database::table_schema(name)`'s column count),
  `reltuples` (always `-1`, meaning "unknown", exactly like a table
  PostgreSQL has never `ANALYZE`d), `relhasindex` (always `f` - this
  engine's B+tree indexes are not modeled as `pg_class` rows here),
  `relpersistence` (always `p`, permanent) and `relam` (always `0`).
  Honors the predicates real clients actually send, each applied as an
  independent filter over the row set (`WHERE` is implicitly one big
  `AND`, and this recognizer never sees an `OR`): `relkind in (...)` drops
  every row unless `'r'` is one of the listed kinds, `nspname = '<x>'` or
  `relnamespace = <oid>` drops every row unless it names `public`/`2200`,
  and `relname = '<x>'` or `oid = <n>` narrows to the one matching table.
  Any other predicate (`relhasindex = false`, a `LIKE`, anything this
  module has no parser for) is silently ignored rather than rejected -
  the row set is simply not narrowed by it, which is the "brittle" half of
  this module's contract: a predicate this module cannot understand does
  not make the query fail, it just does not filter.

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
