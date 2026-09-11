# wire

The PostgreSQL wire protocol listener (`docs/adr/0007-postgres-wire-protocol.md`,
`docs/ROADMAP.md`'s M13.2/M13.3): the `pgwire` crate handles wire framing
and the startup/simple-query/extended-query message flows, and this
module supplies the three handler traits `pgwire` needs to drive those
flows against this engine - `pgwire::api::auth::StartupHandler` (via the
no-authentication `NoopStartupHandler` blanket impl; real authentication
is M22), `pgwire::api::query::SimpleQueryHandler`, and (M13.3)
`pgwire::api::query::ExtendedQueryHandler` together with its
`pgwire::api::stmt::QueryParser`. `sql`'s own hand-written parser is
untouched: `pgwire` never sees SQL grammar, only the wire messages that
carry a query string (and, for the extended protocol, bound parameter
values) to and a result set back from this module.

Each accepted TCP connection gets its own `engine::Database` session, via
`Database::connect()` (`crates/engine/src/database.rs:49`) on the shared
`Arc<Database>` `main` hands to `serve`. A session is not `Clone` and its
`execute` takes `&mut self`, but the `pgwire` handler traits this module
implements are called through `&self` - the same shared handler instance
answers every message on a connection - so the session lives behind an
`Arc<Mutex<Database>>` inside `ConnectionState`, one `ConnectionState` per
connection rather than one shared across connections the way `pgwire`'s
own examples share a single handler. The field is an `Arc<Mutex<_>>`
rather than a bare `Mutex<_>` (M13.3) so `ConnectionState::query_parser`
can clone the same session handle into a separate `StatementParser`
(below) without needing an `Arc<ConnectionState>` to clone from - `&self`
alone is enough. `PgWireServerHandlers::simple_query_handler`,
`::extended_query_handler`, and `::startup_handler` all hand out clones of
the same `Arc<ConnectionState>` so `pgwire` sees one logical handler with
three faces, per the shape `pgwire`'s own `examples/server.rs` uses for a
shared handler.

**Every** call into the session is a blocking call - `connect`, `execute`,
`execute_with_params`, `describe`, and `pg_catalog::answer`, which does not
look like one at the call site but reaches `Database::table_names` and
`Database::table_schema` for `pg_class` and `pg_attribute`
(`pg_catalog.MD`). Each waits on the engine's own worker-pool thread over
an `mpsc` channel (`crates/engine/src/runtime.MD`), so each must run
through `tokio::task::block_in_place` rather than block an async worker
thread directly; the listener is only usable on the multi-thread `tokio`
runtime `block_in_place` requires, which is what `main` builds it as.

That rule is not left to each call site to remember, because one of them
did not: until P-81, `SimpleQueryHandler::do_query` called
`pg_catalog::answer` bare while the four call sites around it were
wrapped, and a `psql` connecting - which introspects `pg_class` before it
shows a prompt - blocked a runtime worker thread and every other
connection's task scheduled on it. The reason it was missed is the reason
prose could not prevent it: `answer` takes a `&Database` and reads like a
string matcher. So the lock and the `block_in_place` are taken together,
once, in `on_session` (below), and every handler goes through it. `serve`'s
own `Database::connect` is the only other `block_in_place` in the file, and
it has no session to lock yet. A new engine call added to this module is
therefore either routed through `on_session` or is visibly unlike the
others.

This subtask (M13.2 subtask 1) wired up the startup handshake:
`AuthenticationOk`, the fixed `ParameterStatus` set `NoopStartupHandler`'s
default `DefaultServerParameterProvider` already sends (`server_version`,
`client_encoding`, `DateStyle`, `TimeZone`, `integer_datetimes`,
`standard_conforming_strings`, and a few more), `BackendKeyData`, and
`ReadyForQuery`.

M13.2 subtask 3 fills in `do_query`: every query string runs through this
connection's session exactly like `cli` or a `#[test]` would call
`Database::execute`, and the resulting `engine::ResultSet` becomes
`RowDescription`/`DataRow`/`CommandComplete` (for `Rows`) or a bare
`CommandComplete` (for `RowsAffected`/`RolledBack`).
`engine::ResultSet` does not say which statement produced it
(`crates/engine/src/result_set.rs`), so `statement_keyword` reads the
query's own leading token(s) - `"CREATE"` looks one word further, so
`CREATE TABLE` and `CREATE INDEX` are told apart from a bare `CREATE` -
after first trimming a single trailing `;` (and the whitespace around
it), since `sql::Parser::parse` only ever accepts a query that reaches
`to_response` with at most one, trailing, and a real client like `psql`
routinely sends one (`BEGIN;`, not `BEGIN`) where `tokio_postgres`'s
`simple_query` in this crate's own tests does not - without the trim,
`"BEGIN;"` never matches the `"BEGIN"` arm below and is misreported as a
bare `Response::Execution` instead of `Response::TransactionStart`, which
also leaves `ReadyForQuery`'s transaction status wrong. `to_response`
uses that keyword together with the `ResultSet` variant
to pick a tag: `INSERT 0 <n>` for `RowsAffected` after an `INSERT`, the
keyword itself verbatim for every other `RowsAffected` (`BEGIN`, `COMMIT`,
`CREATE TABLE`, `CREATE INDEX` - `BEGIN` and `COMMIT` carry that same tag
text but a different `Response` variant since M13.2 subtask 4, see
below), `ROLLBACK` for `ResultSet::RolledBack`, and `SELECT`/`EXPLAIN` for
`Rows` depending on whether the leading keyword was `EXPLAIN`. For `Rows`, `pgwire`'s own `send_query_response`
(`pgwire::api::query`) always appends the number of `DataRow`s actually
streamed to whatever command tag `QueryResponse::set_command_tag` was
given, which is exactly right for `SELECT <n>` but means an `EXPLAIN`'s
`CommandComplete` also carries a row count - unlike real PostgreSQL, whose
`EXPLAIN` tag has no count at all. Matching that exactly would mean
bypassing `pgwire`'s default `on_query` and rebuilding its message
sequence by hand; this module accepts the extra number as a known,
harmless deviation instead.

Column types ride on `engine::ResultSet::Rows`'s `column_types`
(`crates/engine/src/result_set.rs`, plumbed from the binder in M13.2
subtask 2) and `pg_catalog::field_info` (`src/pg_catalog.MD` - moved there
in M13.4 subtask 3, `pub(crate)`, alongside the `pg_type_of` mapping it is
built on) maps each to the fixed PostgreSQL OID the task called for:
`Boolean` to `Type::BOOL` (16), `Integer` to `Type::INT4`
(23), `BigInt` to `Type::INT8` (20), `Double` to `Type::FLOAT8` (701), and
`Varchar(_)`/`None` to `Type::VARCHAR` (25) - `None` (a projection with no
static type, e.g. a bare `NULL` literal) gets the same OID a real `NULL`
literal column gets in PostgreSQL itself. `field_info` takes the wire
format (`FieldFormat`) as a parameter rather than hard-coding it, so its
one caller here (the simple-query path, in `to_response`) keeps passing
`FieldFormat::Text` - behaviour unchanged from before M13.3 - while
`server::wire`'s extended-query handler (M13.3) can build a `FieldInfo`
per the client's own requested format instead. `encode_data_row` builds
each `DataRow` through `pgwire::api::results::DataRowEncoder` rather than
writing length-prefixed bytes by hand: `encode_field` matches each
`types::Value` onto the primitive Rust type `postgres_types::ToSql`/
`ToSqlText` are implemented for (`Boolean(bool)`, `Integer(i32)`,
`BigInt(i64)`, `Double(f64)`, `Varchar(&String)`, and `Null` as
`&None::<i8>` - the placeholder type is never inspected once the value is
`None`, since `Option<T>::to_sql`/`to_sql_text` return "this column is
null" unconditionally, regardless of `T` or of the schema's declared
type), and `DataRowEncoder::encode_field` itself decides text or binary
encoding from the `FieldFormat` on that column's `FieldInfo` - which is
exactly why `field_info` needed a format parameter at all: `DataRowEncoder`
reads it off the schema it was built with, not off anything
`encode_data_row` passes explicitly per call.

M13.4 subtask 1 intercepts real clients' introspection queries even
earlier than that: `do_query` calls `pg_catalog::answer` (`src/pg_catalog.MD`)
through the same `on_session` door every other query path takes, before
`intercept_set_show_reset` ever runs, so a recognized `pg_catalog` query
(`select version()`, `pg_namespace`, `pg_class`, `pg_attribute`, `pg_type`
as of M13.4 subtask 3) never reaches `sql::Parser` at all - that grammar
has no notion of any `pg_*` relation and would fail every one
of them with `42P01 undefined_table`, which is exactly what killed a real
client's connection before this subtask. `db.table_names()`/
`db.table_schema` reach `pg_catalog::answer` through the same
`&Database` the caller already holds under the session lock - no second
lock, no reentrant call into `Database::execute`. `answer`'s `Some(Introspection)`
becomes a `Response::Query` via `introspection_response`, built the same
way `to_response` builds an ordinary `SELECT`'s response - `FieldInfo` per
`Introspection::columns` entry (`FieldFormat::Text`, since the simple
query protocol always answers in text) and one `DataRow` per
`Introspection::rows` entry via `encode_introspection_row`, which encodes
a `None` cell as `None::<&str>` exactly like `encode_field` does for a
`types::Value::Null`. `answer` returning `None` falls through to
`intercept_set_show_reset` and then the engine, unchanged.

M13.2 subtask 5 intercepts `SET`, `RESET` and `SHOW` before a query string
ever reaches `engine::Database::execute` - pgjdbc sends
`SET extra_float_digits` during its own handshake, and none of the three
keywords exists in `sql`'s token list or grammar, so letting them fall
through would surface as a syntax error. `do_query` calls
`intercept_set_show_reset` first; on `Some`, that response is returned
directly and the query is never handed to the session at all. Matching is
case-insensitive on the leading keyword only - `intercept_set_show_reset`
upper-cases only the first whitespace-separated word after trimming a
trailing `;`, so `SET`/`set`/`Set` are all recognized but a parameter name
is compared byte-for-byte. `SET <anything>` and `RESET <anything>` both
become a bare `Response::Execution` with the tag `SET`/`RESET`,
discarding the rest of the query entirely - there is nothing to change,
since no session state backs either keyword yet. `SHOW <name>` becomes a
one-row, one-column `Response::Query` via `show_response`, which answers
from the exact same fixed parameter table `pgwire`'s own
`DefaultServerParameterProvider::default()` builds for the startup
handshake's `ParameterStatus` messages (`server_version`, `TimeZone`,
`standard_conforming_strings`, and the rest) - `show_response` calls
`ServerParameterProvider::server_parameters` itself rather than
duplicating that table, so the two can never drift. A name absent from
that table (or absent entirely, e.g. a bare `SHOW`) answers with an empty
string rather than an error, exactly like `RowsAffected`/`Rows`'s
`CommandComplete` handling above, `show_response`'s `QueryResponse`
carries the command tag `SHOW`, and the same `pgwire` row-count-appending
behaviour noted above for `EXPLAIN` applies to it too.

A simple query carries exactly one statement:
`sql::Parser::parse` (`crates/sql/src/parser.rs`) accepts one optional
trailing semicolon and rejects anything after it, so a multi-statement
simple query (`SELECT 1; SELECT 2;`) surfaces to the client as a syntax
error rather than running both statements. This is a known limitation,
also called out in `crates/server/README.md`.

M13.2 subtask 4 fills in the real per-error SQLSTATE, severity and
`ReadyForQuery` transaction status. `to_pg_error` turns a `common::Error`
into a `pgwire::error::ErrorInfo` carrying `Error::sql_state().as_str()`
as the code, the `Display` text as the message, and `Error::severity()`
mapped to the wire's three severities (`Severity::Error` -> `"ERROR"`,
`Severity::Fatal` -> `"FATAL"`, `Severity::Panic` -> `"PANIC"`) - no
`common::Error` variant produces `Severity::Panic` today, so that arm is
untested but kept for when one does. `do_query` returns this as a
top-level `Err(PgWireError::UserError(..))` rather than folding it into
the `Vec<Response>` it succeeds with, which matters for two reasons: it
is what makes `pgwire`'s own `process_error` compare the severity string
against `"FATAL"` and close the connection after the response for a
`Severity::Fatal` error, and it is what makes `process_error` move this
connection's `TransactionStatus` to its error state the same way a
`Response::Error` inside a successful `do_query` would.

`ReadyForQuery`'s transaction status is not tracked by hand in this
module: `engine::runtime::TxnSlot` (`crates/engine/src/runtime.MD`) that
actually drives it is private to `engine`, so this module leans on
`pgwire`'s own per-connection `TransactionStatus` state machine instead,
driving it with the `Response` variants that carry transitions -
`Response::TransactionStart`/`Response::TransactionEnd` alongside the
plain `Response::Execution`/`Response::Query` subtask 3 already used, and
`PgWireError`'s automatic move to the error state on any `Err` `do_query`
returns. `to_response` emits `Response::TransactionStart` for a
successful `BEGIN` and `Response::TransactionEnd` for a successful
`COMMIT` or for `ResultSet::RolledBack` (which covers both an actual
`ROLLBACK` and a `COMMIT` sent while the session's transaction was
already aborted - `engine::runtime`'s `handle_commit` turns that case
into an `Ok(ResultSet::RolledBack)` rather than an error, and this module
follows suit by ending the transaction rather than starting one). Every
other successful statement keeps using `Response::Execution`/
`Response::Query`, which `pgwire` does not use to change transaction
status at all - matching `TxnSlot::Active` staying `Active` across an
ordinary statement. An error inside an explicit transaction becomes
`Response::Error`'s (or the equivalent top-level `Err`'s) move to the
error state, matching `TxnSlot::Aborted`; an error with no explicit
transaction open leaves `TransactionStatus::Idle` alone, since
`to_error_state` is a no-op from `Idle`, matching autocommit's own
implicit transaction being rolled back without ever touching
`session.txn_slot`. The one case this reconstruction does not reproduce
exactly is a nested `BEGIN` sent while already inside a transaction:
`engine::runtime::handle_begin` rejects it with `Error::NestedTransaction`
without touching `session.txn_slot`, so the real engine-side transaction
stays `Active`, but this module reports `TransactionStatus::Error` for it
like any other statement error, because nothing on the wire tells this
module the difference and the task's own description - "a statement
error while in one moves it to failed" - draws no exception for it.

M13.3 adds the extended query protocol (`Parse`/`Bind`/`Describe`/
`Execute`/`Sync`, for `PreparedStatement` support): `ConnectionState` now
also implements `pgwire::api::query::ExtendedQueryHandler`, and
`ConnectionHandlers::extended_query_handler` hands out the same
`Arc<ConnectionState>` `simple_query_handler` does, so a prepared and a
simple statement on one connection share the one session and the one
transaction. `pgwire`'s own default `on_parse`/`on_bind`/`on_execute`/
`on_describe`/`on_sync`/`on_flush`/`on_close` implementations (`pgwire`'s
`query.rs`) do the actual protocol bookkeeping - storing a `StoredStatement`/
`Portal`, tracking `PortalExecutionState`, resuming a suspended portal -
and this module supplies only what they call out to: `query_parser`
(returning a `StatementParser`), `do_query`, and, through `QueryParser`,
`get_parameter_types`/`get_result_schema`, which are what the trait's own
default `do_describe_statement`/`do_describe_portal` call - **this module
never overrides `do_describe_statement`/`do_describe_portal` itself**,
since the default implementations already do exactly what M13.3 needs
once `QueryParser` reports correct types.

A `PreparedStatement` (`ExtendedQueryHandler::Statement`) is an enum with
two variants (M13.4 subtask 5 turned it from a plain struct into one):
`Ordinary { sql, description }`, carrying the original SQL text and the
`engine::StatementDescription` `Database::describe` returned for it at
`Parse` time - the shape every statement had before M13.4 - and
`Introspection { sql, columns }`, carrying the SQL text and the column
list a `pg_catalog::answer` recognized it as having, with no
`StatementDescription` at all, since an introspection query never reaches
`Database::describe`. `StatementParser::parse_sql` tries
`pg_catalog::answer` first, inside the same `on_session` call it already
needed for `Database::describe`: a
recognized query becomes `Introspection` immediately, and only an
unrecognized one falls through to `Database::describe` and `Ordinary` -
exactly mirroring `SimpleQueryHandler::do_query`'s own
`pg_catalog::answer`-before-`execute` ordering, and for the same reason:
`pg_class`/`pg_namespace`/etc. have no meaning to `sql::Parser`, so
`Database::describe` would fail them the same way `Database::execute`
would. A `common::Error` from `Database::describe` still folds into a
`PgWireError` via `to_pg_error` - so a syntax error in a `Parse` message
is reported the same way a syntax error in a simple query is.
`StatementParser::get_parameter_types` returns an empty `Vec` for
`Introspection` (no recognized shape here ever takes a `$n` placeholder)
and, for `Ordinary`, maps each `Option<types::DataType>` in
`description.param_types` through `pg_catalog::pg_type_of`, which is the
same `DataType` -> `pgwire::api::Type` mapping `field_info` already used,
factored out so both share it; `None` (a placeholder
`planner::infer_parameter_types` couldn't resolve) becomes `Type::VARCHAR`,
the same fallback `field_info` already gave an untyped result column.
`get_result_schema` builds one `FieldInfo` per column - from
`description.columns` for `Ordinary` (via `field_info`), or straight from
the already-typed `columns` for `Introspection` (via a bare
`FieldInfo::new`, since those columns are already `pgwire::api::Type`,
not `types::DataType`) - in the caller-supplied `Format` (`None` for
`do_describe_statement`, which has no portal and therefore no result
format yet; `Some(&target.result_column_format)` for `do_describe_portal`,
per `Format::format_for(idx)`) - `column_format` being `None` falls back
to `FieldFormat::Text`, matching `DescribeStatementResponse`'s own job of
describing a statement in the abstract, before any `Bind` has chosen a
format. Crucially, none of this touches `self.session` beyond the one
`pg_catalog::answer`/`Database::describe` call already made at `Parse`
time - `get_parameter_types`/`get_result_schema` read only the cached
`PreparedStatement`, so a `pg_class` query can be `Describe`d before the
table it will eventually report even exists.

`ExtendedQueryHandler::do_query` is the one required method with no
default: it matches on `&portal.statement.statement`
(`Portal<S>.statement: Arc<StoredStatement<S>>`, `StoredStatement<S>.statement: S`,
so `.statement.statement` reaches our own `PreparedStatement` through two
levels of the framework's own wrapping). For `Introspection { sql, .. }`
(M13.4 subtask 5), it calls `pg_catalog::answer(&session, sql)` again,
fresh, right here at `Execute` time rather than reusing anything computed
at `Parse` - that is the whole point: a table created between `Parse` and
`Execute` must show up, and the only way to guarantee that is to never
cache the *rows*, only the column shape `get_result_schema` needed.

`answer` returning `None` here means the statement was recognized as
introspection at `Parse` and is not one now, and P-82 made that reachable
for exactly one reason: a real table of that `pg_`-prefixed name has been
created in between, so `pg_catalog::shadows_a_real_table` now declines the
query (`pg_catalog.MD`). The column shape `Describe` already promised the
client is then the wrong shape, and none of the three things this could do
is free. Running the real query anyway sends `INTEGER` data under a
`VARCHAR` `RowDescription` the client has already cached, which is a
deserialization failure at the driver rather than an error it can read.
Falling back to the `Parse`-time columns with zero rows is the same silent
wrong answer P-82 was filed about, one protocol over. So it returns
`0A000 feature_not_supported` through `cached_plan_changed` - the
condition PostgreSQL reports as "cached plan must not change result type"
in the same situation - and a client that re-prepares gets the real table.
That is also why the old `unwrap_or_else` fallback is gone rather than
kept as a defensive branch; `unwrap_used`/`expect_used` are
workspace-`warn` lints promoted to errors by `-D warnings`, and a `match`
with a real arm on each side satisfies them without inventing an answer.
For
`Ordinary { sql, description }`, the path is exactly what M13.3 shipped:
`param_types` comes from `description.param_types` mapped through
`pg_catalog::pg_type_of`, each of the portal's actual bound parameters
(`portal.parameter_len()`, not `param_types.len()` - the two can disagree
if a client binds a different number of values than the statement
declares, and the engine's own `common::Error::ParameterCountMismatch`
check is what should catch that, not a `Vec` index panic here) is decoded
via `decode_param`, and `Database::execute_with_params` runs through
`on_session` exactly as `SimpleQueryHandler::do_query` calls
`execute`. The `Response` for `Ordinary` comes from the same `to_response`
the simple query path uses, now given `&portal.result_column_format`
instead of the simple query path's fixed `&Format::UnifiedText` - see
`to_response`'s own entry below for why a per-column format has to reach
the `DataRowEncoder`, not just `RowDescription`: the `QueryResponse`
returned here is exactly what a suspended portal later resumes from
(`pgwire::api::portal::Portal::fetch`), so its rows must already be
encoded in the format the client was told to expect, since no second
`RowDescription` is sent once `Execute` starts streaming `DataRow`s. The
`Introspection` branch reuses the same `introspection_response` the
simple-query path built for M13.4 subtask 1, which always encodes in
text - every recognized introspection query is answered the same way
regardless of protocol, since none of them are typed enough for a client
to meaningfully request binary.

M13.3 subtask 6 adds fetch limits: `Execute` carries a `max_rows`, and
when a portal has more rows than that limit, the backend must answer
`PortalSuspended` instead of `CommandComplete`, resuming from the same
portal - without re-running the statement - on the next `Execute`.
`ExtendedQueryHandler::do_query` here still ignores its own `max_rows`
argument and always returns the full `QueryResponse` for the statement,
because `pgwire`'s own default `_on_execute` (`pgwire::api::query`) is
what does the actual pacing: it calls `do_query` exactly once per portal,
on the `Execute` that finds `Portal::state()` still `PortalExecutionState::Initial`,
and transitions it to `Suspended(response)` via `Portal::start`; every
later `Execute` against that same portal name skips `do_query` entirely
and calls `Portal::fetch(max_rows)` again on the stored `QueryResponse`,
which is why the response `do_query` returns has to be self-contained -
this module was already doing that (`to_response`'s rows are collected
into a `Vec<DataRow>` up front and wrapped in `stream::iter`, never a
stream tied to a live engine cursor), so subtask 6 needed no change to
`do_query`/`to_response` at all. `Portal::fetch` pulls up to `max_rows`
items from that stream, moves the portal to `Finished` only once the
stream is actually exhausted, and reports whether it is still suspended;
`_on_execute` sends `PortalSuspended` when it is and `CommandComplete`
(with the row count of just that fetch) only once it is not - so the
command tag reaches the client exactly once, on the final `Execute` for
that portal, and `crates/server/tests/wire_extended_query.rs`'s
`a_portal_suspends_and_resumes_across_fetch_limited_executes` and
`a_fetch_limit_larger_than_the_row_count_completes_in_one_execute` are
what prove it end to end over `tokio_postgres::Transaction::query_portal`.

`decode_param` picks the concrete Rust type to decode into from the
*server's own* inferred type (`pg_catalog::pg_type_of` on
`description.param_types[idx]`, or `Type::VARCHAR` past the end of that
list), never from
`target.parameter_types` (what the client itself declared, if anything,
in `Parse` - used only by the default `do_describe_statement` to choose
what `ParameterDescription` to *report*). The two can disagree - a client
using `Client::prepare_typed` can declare any OID it likes - and when they
do, this module still decodes using its own inference, so a value the
client encoded for its own declared type but that doesn't parse as the
server's real one fails cleanly rather than being silently reinterpreted;
`crates/server/tests/wire_extended_query.rs`'s
`a_wrong_typed_parameter_is_reported_as_an_error_not_a_dropped_connection`
is built on exactly this gap. `Portal::parameter::<T>(idx, pg_type)`
(`pgwire`'s own helper) does the actual decoding, honoring the portal's
own per-parameter text/binary format
(`pgwire::api::portal::Format::is_binary`) automatically - this module
never branches on format itself, only on which Rust type to ask
`parameter` for. A decode failure (`Portal::parameter` returning `Err`,
whatever pgwire's own SQLSTATE for that failure would have been) is
remapped by `parameter_decode_error` into a fresh `PgWireError` carrying
`common::SqlState::PROTOCOL_VIOLATION` (`08P01`) - the same code
`common::Error::ParameterCountMismatch` uses (`planner::parameters.MD`) -
rather than `pgwire`'s own default `22P02`, since a bind value that
doesn't match this server's own type inference is a protocol-level
disagreement between client and server, not an ordinary data exception a
SQL statement raised.

## Key Components

- `serve(listener, db) -> impl Future<Output = ()>` - the listener's main
  loop. Accepts connections forever; a failed `accept` is logged and does
  not stop the loop. Each accepted socket is handed to its own spawned
  `tokio` task, which opens a session (`Database::connect`, via
  `block_in_place`), builds this connection's `ConnectionHandlers`, and
  runs `pgwire::tokio::process_socket` on it. A session-open failure or
  an I/O error partway through a connection's protocol messages is
  logged and only ends that one connection, never the listener.
- `ConnectionState` - private. Holds this connection's session
  (`Arc<Mutex<engine::Database>>`) and implements the three `pgwire`
  traits that do the actual protocol work: `NoopStartupHandler` (no
  overrides - the default no-authentication handshake is exactly what
  subtask 1 wants), `SimpleQueryHandler::do_query`, which offers the query
  to `pg_catalog::answer` and then to `intercept_set_show_reset` before
  running it through the session - the first and third of those through
  `on_session`, the second touching no engine state at all - and turns its
  `Result<ResultSet>` into a `Vec<Response>` with `to_response`, or a
  `PgWireError` with the statement's real SQLSTATE and severity via
  `to_pg_error`; and `ExtendedQueryHandler` (M13.3, see above) -
  `type Statement = PreparedStatement`, `type QueryParser = StatementParser`,
  `query_parser` clones `session` into a fresh `StatementParser`, and
  `do_query` either re-answers an `Introspection` statement fresh from
  `pg_catalog::answer` (M13.4 subtask 5), failing it with
  `cached_plan_changed` if that answer has since become `None`, or decodes
  an `Ordinary` statement's bound parameters and calls
  `Database::execute_with_params`.
- `cached_plan_changed() -> PgWireError` - private. The `0A000
  feature_not_supported` a prepared `Introspection` statement gets at
  `Execute` when `pg_catalog::answer` no longer recognizes it, which since
  P-82 happens when a real table has taken the `pg_`-prefixed name it was
  described against. It carries no SQL text and no table name - it is an
  error handed straight to a client, and the client already knows which
  statement it sent. PostgreSQL reports the same SQLSTATE in the same
  situation, so a driver that already knows to re-prepare on `0A000` needs
  nothing new to recover.
- `on_session(session, f) -> T` - private. The single door from an async
  handler onto the engine: locks the `Mutex<Database>` (recovering a
  poisoned lock rather than propagating the panic, since one statement's
  panic must not kill the connection's remaining messages) and runs `f`
  against the guard inside `tokio::task::block_in_place`. Every handler in
  this file uses it and nothing else takes that lock, so "did this call
  site remember `block_in_place`?" is a question about one function rather
  than about five. That is deliberate: the four call sites that had the
  wrapper and the one that did not (P-81) were textually identical apart
  from the wrapper, and the missing one was the call that least looked like
  an engine call. Note that the lock is taken *before* `block_in_place` and
  held across it, which is correct here and would not be under an `async`
  lock: this is a `std::sync::Mutex`, the guard never crosses an `.await`,
  and one connection's session is contended only by that connection's own
  pipelined messages.
- `PreparedStatement` - private, `Clone` (required by
  `ExtendedQueryHandler::Statement`). An enum (M13.4 subtask 5) with two
  variants: `Ordinary { sql: String, description: engine::StatementDescription }`
  (`Database::describe`'s result, computed once at `Parse` time and
  reused by every later `Bind`/`Describe`/`Execute` against the same
  statement name - every statement before M13.4), and
  `Introspection { sql: String, columns: Vec<(String, pgwire::api::Type)> }`
  (a `pg_catalog::answer`-recognized query's column shape, with no
  `StatementDescription` at all).
- `StatementParser` - private. Holds a clone of `ConnectionState`'s own
  `Arc<Mutex<engine::Database>>` and implements
  `pgwire::api::stmt::QueryParser<Statement = PreparedStatement>`:
  `parse_sql` tries `pg_catalog::answer` first (building `Introspection`
  on a match) and falls back to `Database::describe` (building `Ordinary`)
  otherwise, both inside one `on_session` call, mapping a
  `common::Error` from `Database::describe` through `to_pg_error` on
  failure; `get_parameter_types` returns an empty `Vec` for
  `Introspection` or maps `Ordinary`'s `description.param_types` through
  `pg_catalog::pg_type_of`; `get_result_schema` builds each `FieldInfo`
  from `Introspection`'s own already-typed `columns` or from `Ordinary`'s
  `description.columns` through `field_info`, in the caller-supplied
  `Format` (falling back to `FieldFormat::Text` when `None`) - neither
  branch of either method ever calls back into `self.session`.
- `decode_param(portal, idx, pg_type) -> PgWireResult<Value>` - private.
  Calls `decode_param_bytes`, remapping any error through
  `parameter_decode_error` so a decode failure always carries `08P01`
  regardless of which internal `pgwire` error caused it.
- `decode_param_bytes(portal, idx, pg_type) -> PgWireResult<Value>` -
  private. Matches `*pg_type` onto the concrete Rust type to decode:
  `BOOL`/`INT4`/`INT8`/`FLOAT8` onto `bool`/`i32`/`i64`/`f64` via
  `Portal::parameter::<T>(idx, pg_type)`, anything else onto `String` via
  `Portal::parameter::<String>(idx, &Type::VARCHAR)` (the `VARCHAR`/`TEXT`/
  unknown-type fallback); a `None` from `Portal::parameter` (a SQL `NULL`)
  becomes `Value::Null` via `Option::map_or` rather than an empty string,
  for every branch alike.
- `parameter_decode_error(idx, err) -> PgWireError` - private. Wraps a
  `Portal::parameter` failure in a fresh `PgWireError::UserError` carrying
  `common::SqlState::PROTOCOL_VIOLATION` (`08P01`) and a message naming the
  1-based parameter index, in place of whatever SQLSTATE `pgwire`'s own
  `PgWireError` variant would otherwise have carried (see above for why).
- `to_pg_error(err) -> PgWireError` - private. Builds the
  `pgwire::error::ErrorInfo` (SQLSTATE, severity, message) a
  `common::Error` becomes, wrapped in `PgWireError::UserError`; see above
  for why this is a top-level `Err` and not a `Response::Error`.
- `intercept_set_show_reset(client, query) -> Option<Response>` - private.
  Recognizes a `SET`/`RESET`/`SHOW` query by its leading keyword (folded
  to uppercase) before it reaches the session at all; `None` for every
  other query, which `do_query` then runs through `engine::Database`
  exactly as before.
- `show_response(client, name) -> Response` - private. The one-row,
  one-column `Response::Query` a `SHOW <name>` becomes, its value read
  from `pgwire`'s own `DefaultServerParameterProvider::default()` table -
  the same one the startup handshake's `ParameterStatus` messages use -
  or an empty string when `name` is not in it.
- `introspection_response(introspection) -> Response` (M13.4) - private.
  Builds the `Response::Query` a `pg_catalog::Introspection` becomes: one
  `FieldInfo` per `Introspection::columns` entry (always `FieldFormat::Text`)
  and one `DataRow` per `Introspection::rows` entry via
  `encode_introspection_row`, with the command tag fixed at `SELECT` -
  every recognized introspection query is a `SELECT`.
- `encode_introspection_row(encoder, row) -> PgWireResult<DataRow>` (M13.4) -
  private. Encodes one `Introspection` row cell by cell: `Some(value)`
  through `DataRowEncoder::encode_field`, `None` as `None::<&str>` (a SQL
  `NULL`), mirroring `encode_field`'s own `Value::Null` arm.
- `to_response(query, result_set, result_format) -> Response` - private.
  Builds the `pgwire::api::results::Response` a `ResultSet` becomes:
  `Response::Query` (`RowDescription` + one `DataRow` per row, via
  `field_info` and `encode_data_row`) for `Rows`, each column's
  `FieldFormat` taken from `result_format.format_for(idx)` (M13.3) - the
  simple-query call site passes `&Format::UnifiedText` (unchanged
  behavior), the extended-query call site passes
  `&portal.result_column_format`; for `RowsAffected`,
  `Response::TransactionStart` for a `BEGIN`, `Response::TransactionEnd`
  for a `COMMIT`, and `Response::Execution` with a tag from
  `statement_keyword` for everything else; `Response::TransactionEnd` for
  `RolledBack` too, whichever of `ROLLBACK` or an aborted `COMMIT`
  produced it.
- `statement_keyword(sql) -> String` - private. The query's leading
  keyword, upper-cased, after trimming a single trailing `;`; `CREATE`
  looks one further word ahead so `CREATE TABLE`/`CREATE INDEX` read as
  two-word keywords.
- `encode_data_row(encoder, tuple) -> PgWireResult<DataRow>` - private.
  Encodes one `types::Tuple` into `encoder` field by field via
  `encode_field`, then calls `DataRowEncoder::take_row` to produce the
  `DataRow` and reset the encoder's internal buffer for the next row -
  `to_response` builds one `DataRowEncoder` per query and reuses it across
  every row rather than allocating a fresh one each time.
- `encode_field(encoder, value) -> PgWireResult<()>` - private. Matches one
  `types::Value` onto the concrete Rust type `DataRowEncoder::encode_field`
  needs and calls it: `Boolean`/`Integer`/`BigInt`/`Double`/`Varchar` onto
  their obvious Rust equivalents, `Null` onto `&None::<i8>` (an arbitrary
  placeholder type - `Option<T>` encodes "null" the same way for any `T`).
- `ConnectionHandlers` - private. The `pgwire::api::PgWireServerHandlers`
  a connection is served with; all three of its methods return a clone of
  the same `Arc<ConnectionState>`, so `startup_handler`,
  `simple_query_handler`, and `extended_query_handler` are all backed by
  the one session.

## Usage Example

```rust
use std::sync::Arc;

use engine::Database;
use tokio::net::TcpListener;

async fn run(db: Arc<Database>) -> std::io::Result<()> {
    let listener = TcpListener::bind("127.0.0.1:5432").await?;
    server::wire::serve(listener, db).await;
    Ok(())
}
```

`crates/server/src/main.rs` builds a multi-thread `tokio` runtime after
`Database::open` succeeds, binds the configured `--pg-addr` listener on
it, and spawns `serve` as a task on that runtime; `crates/server/tests/wire_startup.rs`
drives the same function against an ephemeral `127.0.0.1:0` listener and
a `tempfile` database, and proves the handshake completed by connecting
with `tokio_postgres` and observing a successful `connect()`.
`crates/server/tests/wire_simple_query.rs` drives the same listener with
`tokio_postgres::Client::simple_query`, proving `CREATE TABLE`, `INSERT`,
`SELECT`, `BEGIN`/`COMMIT` and `EXPLAIN` all round-trip through this
module's `RowDescription`/`DataRow`/`CommandComplete` handling.
`crates/server/tests/wire_errors.rs` proves `to_pg_error` and the
`TransactionStatus` reconstruction above: an undefined table's SQLSTATE
reaches `tokio_postgres` as `42P01`, a failing statement inside an
explicit transaction leaves the next statement rejected with `25P02`
until `ROLLBACK`, and the connection is still usable afterward.
`crates/server/tests/wire_set_show_reset.rs` proves
`intercept_set_show_reset`: `SET`/`RESET` are accepted as no-ops, `SHOW`
of a known and an unknown parameter both succeed, the leading keyword is
recognized regardless of case, and the connection remains usable for an
ordinary statement afterward.
`crates/server/tests/wire_extended_query.rs` drives the same listener
with `tokio_postgres::Client::prepare`/`prepare_typed`/`query`/`execute`,
proving `ExtendedQueryHandler`/`QueryParser`: bound `bool`/`i32`/`i64`/
`f64`/`&str` parameters round-trip through `decode_param`, a `NULL`
parameter stores a real SQL `NULL`, one prepared statement runs
repeatedly with different values, `BEGIN`/`COMMIT` (over `simple_query`)
wrap a prepared `INSERT` on the same session, a parameter whose bytes
don't decode as the server's own inferred type comes back as an `08P01`
error rather than a dropped connection, and (M13.3 subtask 6) a portal
fetched in limited batches via `Transaction::query_portal` suspends and
resumes correctly, returning the same rows exactly once and completing in
a single `Execute` when the limit exceeds the row count.
`crates/server/tests/wire_pg_catalog.rs` proves `pg_catalog::answer`'s
interception (M13.4) over both protocols: `select version()`,
`pg_namespace`, `pg_class`, `pg_attribute`, `pg_type` and any other
`pg_`-prefixed relation all answer correctly over `simple_query`, an
ordinary `SELECT` is unaffected, and (subtask 5) preparing a `pg_class`
query over the extended protocol succeeds with zero reported parameters
and still sees a table created *after* `Parse`, proving
`ExtendedQueryHandler::do_query`'s `Introspection` branch recomputes its
answer at `Execute` time rather than reusing anything cached at `Parse`.
