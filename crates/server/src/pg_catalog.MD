# pg_catalog

Intercepts the introspection queries a real PostgreSQL client library sends
before it will trust a connection - `select version()`, `pg_namespace`,
`pg_class` (M13.4 subtask 2), `pg_attribute` and `pg_type` (M13.4
subtask 3), `pg_database`, `pg_roles` and `pg_user` (P-90), and any other
`pg_`-named relation at all (M13.4 subtask 4) -
and answers them from this engine's own live catalog rather than letting
them reach `sql::Parser`, which knows nothing about any relation named
`pg_*` and would fail every one of them with `42P01 undefined_table`,
killing the client's connection before its first real statement.
`crates/server/src/wire.rs` calls `answer` at the very front of
`SimpleQueryHandler::do_query`, ahead of `intercept_set_show_reset`, so a
recognized introspection query never reaches `engine::Database::execute`
at all; a query whose `FROM` names something that is neither `pg_`-prefixed
nor one of this module's own recognized shapes falls through unchanged -
`SELECT * FROM nosuchtable` still reaches the engine and still fails with
a real `42P01`, exactly as it would if this module did not exist.

**A relation the engine really has is never intercepted, whatever it is
called.** `sql`'s grammar accepts `CREATE TABLE pg_foo (...)`, so a user
can own a table whose name this module's `pg_`-prefix rule would otherwise
claim; before P-82 the catch-all shadowed it and `SELECT * FROM pg_foo`
answered zero rows under a `?column?` heading while the `INSERT`s into it
had all reported success. That is the worst shape a wrong answer can take,
and it was a silent one. So `answer` now asks the engine first, in
`shadows_a_real_table`, and returns `None` for any unqualified
`pg_`-prefixed name that `Database::table_names` knows: the real table
wins, and every recognizer behind that check - including the four named
ones - inherits the rule without restating it. The question this module
never used to ask is whether the engine already had an answer, and that is
the general hazard of an interception layer sitting in front of a real
one.

The one name that stays intercepted is an explicitly qualified
`pg_catalog.pg_foo`. Writing the schema out asks for the catalog by name
rather than for whatever `pg_foo` currently resolves to, which is how
PostgreSQL reads it too, and falling through would hand `sql::Parser` a
dotted relation name its grammar has no meaning for. Reserving the `pg_`
prefix at `CREATE TABLE` time, the way PostgreSQL does, was the other way
to close this; it was rejected here because it is a `sql`/`catalog` change
that would make the reservation a property of the engine rather than of
this compatibility shim, and the milestone that wants a reserved prefix
can still make that decision later without unpicking this one.

The
extended query protocol (M13.4 subtask 5) calls `answer` twice for the
same statement, deliberately: once in `StatementParser::parse_sql` at
`Parse` time, purely to learn the column shape `Describe` needs to report
without ever calling `Database::describe` (which would fail a `pg_class`
query the same way `Database::execute` would), and again in
`ExtendedQueryHandler::do_query` at `Execute` time, to compute the actual
rows against whatever the catalog looks like *then* - so a table created
between `Parse` and `Execute` still shows up in a `pg_class` listing,
exactly as it would for an ordinary `SELECT` reaching the real engine.

The existence check above gives that second call a case it did not have
before: a statement `Parse` described as introspection can be `Execute`d
after a real table of that name has appeared, at which point `answer`
returns `None` and the shape `Describe` already promised the client is no
longer the shape the query has. `wire.rs` answers that with
`0A000 feature_not_supported`, the same condition PostgreSQL reports as
"cached plan must not change result type", rather than running the real
query under the old column types or falling back to zero rows - see
`wire.MD`'s `cached_plan_changed`.

This module also carries `pg_type_of`/`field_info` (moved here from
`wire.rs` in M13.4 subtask 3, `pub(crate)` since `wire.rs`'s own
`to_response`/`StatementParser` still need them for an ordinary result
set's column types): the `types::DataType` -> `pgwire::api::Type` mapping
every recognizer that reports a real column's type reuses, so
`pg_attribute`'s `atttypid` and an ordinary `SELECT`'s `RowDescription`
can never disagree about what oid a given `DataType` maps to.

**Zero rows is the answer for a relation nothing backs - except for the
three that decide whether the client has a tree to draw at all.** A
client reads an empty `pg_database` as "this server serves no databases"
and stops there, never asking about the schemas and tables the
recognizers above could already list, which is exactly what P-90
reported: a connection that worked, a console that ran statements, and an
empty object tree above it. So `pg_database`, `pg_roles` and `pg_user`
are answered from the server's own *configuration* rather than from a
catalog - one database row whose `datname` is
`engine::Database::database_name()` (the `db_path` file stem, computed
once when the database is opened), and one role row for the single
implicit superuser every connection already is, since nothing
authenticates until M22. These are the only fabricated rows in this
module, and they are fabricated from something real and fixed: no value
in them is invented per query, and the oids are derived the same
deterministic way `table_oid` derives a table's, so a client that caches
one gets the same answer after a reconnect.
`docs/adr/0021-one-database-one-role-until-m22-and-m24.md` records the
decision and, more usefully for whoever adds the next recognizer, the
test that bounds it: a relation a client uses to decide whether anything
exists is answered from configuration, and a relation it uses to describe
something it already found is answered from the catalog or with zero
rows.

Everywhere else the principle still holds - where nothing backs an
answer, the answer is zero rows. The general
fallback `answer_unrecognized_pg_relation` (M13.4 subtask 4) is where that
principle is most visible, since it recognizes the *shape* of any
`pg_`-prefixed relation without knowing anything at all about what it
means, and answers zero rows of whatever columns the query itself asked
for rather than guessing at either. It is deliberately brittle text
matching against a normalized query string, not a real SQL parser -
`sql`'s own grammar has no notion of `pg_catalog` relations at all, and
teaching it one is out of scope here (`docs/ROADMAP.md`'s M24 is where
real `pg_catalog` tables, answered through actual joins, eventually
replace this module - `answer_unrecognized_pg_relation` most of all, since
it is the one recognizer here that answers correctly only by coincidence
of always saying "nothing").

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
- `answer(db, sql) -> Option<Introspection>` - normalizes `sql`, gives
  `shadows_a_real_table` first refusal, and then tries
  each recognizer in turn (`select version()`, then `pg_namespace`, then
  `pg_class`, then `pg_attribute`, then `pg_type`, then `pg_database`,
  `pg_roles` and `pg_user`, then the
  `answer_unrecognized_pg_relation` catch-all, which must stay last or it
  claims every relation the specific recognizers were added for),
  returning the first
  match's `Introspection` or `None` when nothing recognizes it, in which
  case the caller runs the query through the engine exactly as before.
- `shadows_a_real_table(db, normalized) -> bool` - private. The one
  question every recognizer in this module used to skip: does the engine
  already have a table by this name? `true` only when the `FROM` token is
  unqualified, `pg_`-prefixed, and matches a `Database::table_names()`
  entry case-insensitively, in which case `answer` returns `None` and the
  query goes to the engine like any other. It sits in `answer` rather than
  in each recognizer on purpose - a check repeated six times is a check
  the seventh recognizer will not have - and it costs one
  `Database::table_names()` round trip, paid only by a query whose `FROM`
  is `pg_`-prefixed and never by an ordinary statement. An explicitly
  `pg_catalog.`-qualified name is excluded: it names the catalog rather
  than whatever the bare name resolves to, and the engine's grammar could
  not run it anyway.
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
- `pg_type_of(data_type) -> Type` - `pub(crate)`. The `types::DataType` ->
  `pgwire::api::Type` mapping (`Boolean`/`Integer`/`BigInt`/`Double` to
  their obvious OIDs, `Varchar(_)`/`None` to `Type::VARCHAR`); moved here
  from `wire.rs` (M13.4 subtask 3) since `answer_pg_attribute` below needs
  it as much as `wire.rs`'s own `StatementParser`/`to_response` do, and a
  second copy would risk the two disagreeing about a type's oid.
- `field_info(name, data_type, format) -> FieldInfo` - `pub(crate)`. Builds
  the `FieldInfo` an ordinary `RowDescription` needs from `pg_type_of` and
  a caller-supplied `FieldFormat`; moved here alongside `pg_type_of` for
  the same reason, `wire.rs`'s `to_response`/`StatementParser` are its only
  callers today.
- `answer_pg_attribute(db, normalized) -> Option<Introspection>` -
  private. Recognizes a query whose `FROM` names `pg_attribute` and
  answers one row per column of every table in `Database::table_names()`
  (or, once an `attrelid = <oid>` predicate narrows it, just the one table
  that oid names): `attrelid` (`table_oid`), `attname`, `atttypid`
  (`pg_type_of(&column.data_type).oid()`), `attnum` (1-based position
  within `Schema::columns()`, so this recognizer's own row order is
  already `attnum` order without needing to sort), `attnotnull` (`t` when
  `!column.nullable`), `atttypmod` (always `-1`), `attisdropped` (always
  `f` - this engine has no `DROP COLUMN`) and `attidentity` (always empty
  - no identity columns). An `attname = '<x>'` predicate further narrows
  the row set to that one column, once `attrelid`/table selection has
  already run; `attnum > 0` is not parsed at all, since every column this
  engine can report already satisfies it (no system columns exist to
  exclude).
- `pg_type_catalog() -> Vec<Type>` - private, pure. The five
  `pgwire::api::Type`s this engine's `DataType` ever maps to
  (`BOOL`/`INT4`/`INT8`/`FLOAT8`/`VARCHAR`), in a fixed order - the
  complete `pg_type` row set before any `WHERE` filter narrows it.
- `answer_pg_type(normalized) -> Option<Introspection>` - private.
  Recognizes a query whose `FROM` names `pg_type` and answers one row per
  `pg_type_catalog()` entry: `oid`/`typname` from the `Type` itself
  (`Type::oid()`/`Type::name()`, so this can never disagree with
  `pg_type_of`'s own mapping), `typnamespace` (always `11`, `pg_catalog`),
  `typtype` (always `b`, base type), and `typelem`/`typbasetype`/`typrelid`
  (always `0` - none of this engine's five types is an array, a domain, or
  a composite type). `oid = <n>` and `typname = '<x>'` predicates narrow
  the row set; a `typname` this engine has no `DataType` for - `lo`, or any
  real PostgreSQL type name this engine doesn't model - answers zero rows
  rather than an error, exactly like `answer_pg_class`'s `relname` filter
  does for a table that doesn't exist.
- `bool_cell(value) -> Option<String>` - private, pure. `t` or `f`, the
  text-format encoding of a PostgreSQL `bool`. The three recognizers
  below declare their boolean columns as `Type::BOOL` rather than
  `Type::VARCHAR`, which the older recognizers use, because
  `wire.rs`'s `introspection_response` always encodes a cell in text
  format and `t`/`f` is exactly what a text-format `bool` is on the wire -
  so the declared type and the bytes agree, and a driver asking for a
  boolean gets one.
- `database_oid(name) -> u32` - private, pure. `table_oid` over
  `"pg_database.<name>"`, so a database and a table of the same name
  never collide on one oid while both stay deterministic across restarts.
- `answer_pg_database(db, normalized) -> Option<Introspection>` -
  private. Recognizes a query whose `FROM` names `pg_database` and
  answers exactly one row, because one server process serves exactly one
  database file: `oid` (`database_oid`), `datname`
  (`Database::database_name()`), `datdba` (`10`, the same fixed owner oid
  `answer_pg_namespace` reports and the oid `answer_pg_roles` below
  actually lists), `encoding` (`6`, `UTF8`), `datcollate`/`datctype`
  (`C`), `datistemplate` (`f`), `datallowconn` (`t`), `datconnlimit`
  (`-1`, no limit), `dattablespace` (`1663`, real PostgreSQL's
  `pg_default`) and `datacl` (`NULL`). A `datname = '<x>'` or `oid = <n>`
  predicate that does not match this one database answers zero rows;
  every other predicate is ignored rather than rejected, exactly as in
  `answer_pg_class`.
- `answer_pg_roles(normalized) -> Option<Introspection>` - private. One
  row for the single implicit superuser: `oid` `10`, `rolname`
  `postgres`, every capability flag (`rolsuper`, `rolinherit`,
  `rolcreaterole`, `rolcreatedb`, `rolcanlogin`, `rolreplication`,
  `rolbypassrls`) `t`, `rolconnlimit` `-1`, `rolvaliduntil`/`rolconfig`
  `NULL`. The name is fixed rather than echoed from the client's startup
  message on purpose: nothing authenticates until M22, so the connecting
  user's name is unverified text, and a `pg_namespace.nspowner` of `10`
  has to resolve to a role this recognizer actually lists. A
  `rolname = '<x>'` or `oid = <n>` predicate naming anything else answers
  zero rows.
- `answer_pg_user(normalized) -> Option<Introspection>` - private. The
  same single role under `pg_user`'s own column names - `usename`,
  `usesysid` (`10`), `usecreatedb`/`usesuper`/`userepl` (`t`), `passwd`
  (`********`, never a credential), `valuntil`/`useconfig` (`NULL`) - and
  filtered on `usename`/`usesysid`. It is a separate recognizer rather
  than an alias of `answer_pg_roles` because a client selecting `usename`
  from a `pg_roles`-shaped row set would find no such column.
- `is_pg_relation(token) -> bool` - private, pure. `true` for a `FROM`
  token that is `pg_`-prefixed, either bare (`pg_index`) or
  `pg_catalog.`-prefixed (`pg_catalog.pg_settings`); the guard for
  `answer_unrecognized_pg_relation` below, run only after every specific
  recognizer above has already had first refusal.
- `select_list(normalized) -> Option<&str>` - private, pure. The raw text
  between a leading `select ` and the first ` from `, trimmed; `None` for
  a query this module's `normalize` didn't produce in that shape (there
  is always one here, since every caller of this function already matched
  a `FROM`, but the type stays honest about it).
- `split_select_items(select_list) -> Vec<&str>` - private, pure. Splits a
  select list on top-level commas - tracking `(`/`)` depth and
  single-quoted strings so a comma inside a function call's argument list
  or a string literal is not mistaken for an item separator - and trims
  each resulting item.
- `is_simple_identifier(item) -> bool` - private, pure. `true` for one or
  more `.`-joined identifier segments (`oid`, `t.relname`), each starting
  with a letter or underscore and continuing with letters, digits or
  underscores; `false` for anything with an operator, a function call, a
  literal, or `*` in it.
- `select_item_name(item) -> String` - private, pure. The column name this
  module reports for one select-list item, per the fixed rule the module
  header describes: an ` as <alias>` suffix names the alias; otherwise a
  plain identifier `x` or a qualified `t.x` names `x` (the part after the
  last `.`); anything else, `*` included, names the literal string
  `?column?` - real PostgreSQL's own label for an unnamed projection.
- `select_list_columns(normalized) -> Vec<(String, Type)>` - private,
  pure. Maps every item `split_select_items` finds through
  `select_item_name`, each column reported as `Type::VARCHAR` - the
  column *names* matter for a driver's `RowDescription` bookkeeping, but
  since every row set from this recognizer is empty, no cell is ever
  actually encoded in that type.
- `answer_unrecognized_pg_relation(normalized) -> Option<Introspection>` -
  private. The M13.4 subtask 4 catch-all: once every specific recognizer
  above has passed, a `FROM` naming any other `is_pg_relation` token -
  `pg_index`, `pg_constraint`, `pg_description`, `pg_proc`,
  `pg_settings`, or anything else starting `pg_` - answers zero rows with
  columns named from the query's own select list
  (`select_list_columns`), rather than reaching `sql::Parser` and failing
  with `42P01 undefined_table`. A relation name that is *not* `pg_`-prefixed
  (an ordinary missing table, `nosuchtable`) is not this recognizer's
  concern at all - `is_pg_relation` returns `false` and `answer` falls
  through to the engine, which reports the real `42P01` exactly as if this
  module were not in the path. A `pg_`-prefixed name that *does* exist as a
  real table never reaches this function either: `answer` has already
  returned `None` for it in `shadows_a_real_table`.

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
type system. The extended query protocol (M13.4 subtask 5) calls `answer`
from two different places in `wire.rs`: `StatementParser::parse_sql`
(caching only the columns, in a `PreparedStatement::Introspection`) and
`ExtendedQueryHandler::do_query` (recomputing the full answer at `Execute`
time). `crates/server/tests/wire_pg_catalog.rs` drives both protocols end
to end, the simple query protocol over `tokio_postgres::Client::simple_query`
and the extended one over `Client::prepare`/`Client::query`.
