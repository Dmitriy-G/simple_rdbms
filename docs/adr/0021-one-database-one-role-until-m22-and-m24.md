# ADR 0021: One database and one role, fabricated from configuration

Date: 2026-09-11

Status: Accepted

## Context

M13.4 answers a client's `pg_catalog` introspection by matching known
query shapes and reading the real catalog, and it set itself a rule that
kept the interception honest: *never fabricate a row for data the engine
does not have*. Where nothing backs an answer, the answer is zero rows —
which is what `answer_unrecognized_pg_relation` gives every `pg_`-named
relation no recognizer claims.

That rule turned out to have an exception it could not see. A client does
not read an empty relation as "this server does not model that"; it reads
it as the answer. An empty `pg_database` means "this server serves no
databases", and a client that believes that never asks the next question.
P-90 reported exactly that from a DataGrip session against the M13.4
tree: the connection worked, statements ran in the console, and the
object tree was empty — not because `pg_namespace` and `pg_class` could
not answer, but because nothing above them said there was a database to
expand. The levels below the root were already in place and unreachable.

So `pg_database`, `pg_roles` and `pg_user` are different in kind from
`pg_index` or `pg_settings`. A client merely *decorates* its tree with
the latter; it decides whether the tree has a root at all from the
former. Zero rows is a correct non-answer for one and a wrong answer for
the other.

Two constraints shaped what could be done about it. There is no catalog
of databases or roles to read from: one server process opens exactly one
database file (`common::DbConfig::db_path`) and there is no `CREATE
DATABASE`, and there is no authentication of any kind, so every
connection is already an implicit superuser — the M22 entry in
`docs/ROADMAP.md` is what changes that. And M24, which replaces this
whole interception layer with real `pg_catalog` tables answered by
ordinary queries, sits behind M23.1's joins, several milestones away.
Waiting for it meant every client's object tree stayed empty in the
meantime, which is precisely the gap M13.4 exists to close.

## Decision

**`pg_database`, `pg_roles` and `pg_user` are answered from the server's
own configuration, one row each, and they are the only fabricated rows in
`crates/server/src/pg_catalog.rs`.**

- The database's name is `engine::Database::database_name()` — the file
  stem of `DbConfig::db_path`, computed once when the database is opened
  and stored on the handle. It is defined in the engine, not in `server`,
  so nothing downstream re-derives a name from a path and disagrees; the
  session-context expressions of P-91 will want the same value.
- The role is fixed: oid `10`, name `postgres`, every capability flag
  true. It is deliberately **not** the user name the client sent in its
  startup message. Nothing authenticates that name, so echoing it would
  present unverified text as an identity, and the oid `10` that
  `answer_pg_namespace` already reports as `nspowner` has to resolve to a
  role the server actually lists.
- Fabricated oids are derived the way `table_oid` derives a table's — a
  deterministic FNV-1a hash — so an oid a client caches is still valid
  after it reconnects, without this engine persisting an oid anywhere.
- The M13.4 rule stands everywhere else. A relation whose data the engine
  does not have still answers zero rows, and the catch-all still runs
  last.

The exception is stated as a test rather than as a list, so the next
relation a client needs can be judged by it: **a relation a client uses
to decide whether anything exists is answered from configuration; a
relation it uses to describe something it already found is answered from
the catalog, or with zero rows.**

## Consequences

A real client's object tree resolves: a database node, its two schemas,
and the tables under `public`, all from recognizers that were already
there except for the root. `crates/server/tests/wire_pg_catalog.rs`
asserts row *content* for each of the three — the file stem in `datname`,
the role oid matching `pg_namespace.nspowner` — because the zero-row
catch-all means a query that merely succeeds proves nothing about which
code answered it.

What was given up is that a client showing object ownership sees
`postgres`, whoever connected, and a client that asks "which databases
are on this server" is told one, correctly but incidentally: the answer
would still be one if this process could serve several. Both are
consequences of the same missing subsystems rather than of this decision,
and both have an owner. **M22 supplies the real role**: once
authentication exists, the fixed name and oid here become the session's
authenticated identity, and the `passwd` cell — `********` today, never a
credential — becomes a value that must stay redacted for a real reason.
**M24 deletes all three recognizers**, along with the rest of the
interception table, once `pg_catalog` tables are answered by ordinary
queries over real catalog state.

The cost of getting this wrong is worth naming, because it is the reason
the rule above is written as a test: fabricating a row is a silent wrong
answer, the same failure mode P-82 fixed from the other direction (a real
table shadowed by the catch-all). Three relations that cannot be wrong —
one database that is the one open file, one role that is the one implicit
superuser — are a bounded exception. A fourth relation added by analogy,
without the test above, is how it stops being one.
