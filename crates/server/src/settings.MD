# settings

The configuration parameters this server reports to a client, in one
table, read by both of the things that can report them: `pg_catalog`'s
`pg_settings` recognizer and `wire`'s `SHOW` handler. Two readers and one
table is the whole point of the module - a client that asks the same
question two ways (`SHOW server_version_num` and `select setting from
pg_settings where name = 'server_version_num'`) must get the same answer,
and the only reliable way to guarantee that is to leave it no opportunity
to differ.

It exists because of P-94. A client reads a server version number before
it decides which introspection queries it may send, and it consumes that
answer as a *single value*: a driver doing `Number.longValue()` on an
empty result set gets a null and throws, killing the connection with a
message that names no SQL at all. Until this module, `pg_settings` fell
through to `pg_catalog`'s zero-row catch-all, which is a correct
non-answer for a relation a client merely lists and a fatal one for a
relation it reads a scalar from
(`docs/adr/0021-one-database-one-role-until-m22-and-m24.md`).

Every value here is fixed text rather than anything measured, and two of
them are load-bearing. `server_version` and `server_version_num` must
agree with each other and with the `PostgreSQL 15.0` string the engine's
`version()` returns - `15.0` is `150000` in the numeric form, and a
client that reads one and parses the other will gate features on the
result. `transaction_isolation` reports `read committed`, which is what
this engine's snapshot reads present as to a client that asks; see
`docs/adr/0004-acid-scope.md` for what that does and does not promise.

What this module deliberately does not do is enumerate PostgreSQL's
several hundred settings. A `pg_settings` query for a name outside this
table still answers zero rows, exactly as a query against a relation this
engine does not model does. The table covers what clients actually read;
the guard against the next gap is not a longer list but the log line
`wire.rs` now emits for every query it answers without the engine (P-93),
which carries the shape and the row count, so a zero-row answer to a
scalar question is one line to find rather than an investigation.

## Key Components

- `SETTINGS` - the table itself, `&'static [(&str, &str)]` of name and
  value, kept in case-insensitive alphabetical order so a reader can see
  at a glance what is and is not covered. Names keep PostgreSQL's own
  spelling, including the mixed case of `DateStyle`, `IntervalStyle` and
  `TimeZone`, since that is how they appear in a `pg_settings` listing.
- `TRANSACTION_ISOLATION` - the `transaction_isolation` name as a
  constant, because `parameter_name` below has to produce it from a
  phrase rather than from a word and a typo between the two would be
  silent.
- `lookup(name) -> Option<&'static str>` - the value for a parameter
  name, matched case-insensitively (PostgreSQL treats GUC names that
  way), or `None` for a name this server does not report. `None` is the
  caller's decision to make: `SHOW` turns it into `pgwire`'s own
  parameter table and then an empty string, while `pg_settings` simply
  has no row for it.
- `parameter_name(words) -> String` - the parameter a `SHOW` names, from
  the words following the keyword. Almost always the first word, with one
  exception that matters: `SHOW TRANSACTION ISOLATION LEVEL` is
  PostgreSQL's own spelling of a query for `transaction_isolation`, and a
  JDBC driver sends exactly that for `getTransactionIsolation()`. Taking
  the first word there would look up `transaction`, find nothing, and
  answer an empty string - a wrong answer rather than a missing one.

## Usage Example

```rust
use crate::settings;

let value = settings::lookup("server_version_num");
assert_eq!(value, Some("150000"));

let name = settings::parameter_name(&["transaction", "isolation", "level"]);
assert_eq!(name, "transaction_isolation");
```

`crates/server/src/pg_catalog.rs`'s `answer_pg_settings` maps `SETTINGS`
into one `Introspection` row per entry, and
`crates/server/src/wire.rs`'s `show_response` calls `lookup` before
falling back to `pgwire`'s `DefaultServerParameterProvider`, which is the
table the startup handshake's `ParameterStatus` messages come from and
which has no `server_version_num` in it at all.
