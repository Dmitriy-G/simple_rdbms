# ADR 0021: One database and one role, fabricated from configuration

Date: 2026-09-11

Revised: 2026-09-11 — Decision and Consequences: the test this ADR stated
covered only relations a client *lists*, and a client that reads a single
value out of one crashed on the empty answer (P-94). `pg_settings` joins
the relations answered from configuration, and the scalar rule below is
the generalization.

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

**`pg_database`, `pg_roles`, `pg_user` and `pg_settings` are answered
from the server's
own configuration, and they are the only fabricated rows in
`crates/server/src/pg_catalog.rs`.** The first three answer one row each;
`pg_settings` answers one row per parameter in
`crates/server/src/settings.rs`, which is also the table `SHOW` reads, so
the two spellings of one question cannot disagree.

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

**That test was too narrow, and the revision above is what P-94 cost.**
It asks what a client does with a *relation* — lists it, or decides
something exists from it — and says nothing about what the client does
with the *answer*. A driver that runs
`select setting from pg_settings where name = 'server_version_num'` is
not listing anything: it is reading one value, and it reads the empty
result set as a null, not as an empty list. The failure that produced
this revision was a Java driver calling `Number.longValue()` on that
null, which killed the connection with a message naming no SQL at all —
strictly worse than an error, because an error at least names the
relation. So the test has a second half:

**A query whose answer a client consumes as a single value must never be
answered with zero rows. Answer it, or let it fail loudly.**

The two halves differ in what they protect against. The first is about a
client *misreading* a correct-looking answer — an empty `pg_database`
means "no databases", so the tree stays empty. The second is about a
client *crashing* on one, and it is the more dangerous of the two because
the crash names nothing: no SQLSTATE, no relation, no statement. Zero
rows remains the right answer for everything else, and the catch-all
still runs last.

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
**M24 deletes all four recognizers**, along with the rest of the
interception table, once `pg_catalog` tables are answered by ordinary
queries over real catalog state.

`pg_settings` brought a consequence of its own, and it is a limit rather
than a cost: a query for a parameter outside
`crates/server/src/settings.rs` still answers zero rows, so the scalar
rule is satisfied for the parameters clients actually read and not by
construction. Lengthening that table is not the guard — **the guard is
that the answer is now visible**. Since P-93, every query the wire layer
answers without the engine logs its shape and its row count, so the next
zero-row answer to a scalar question is one line in the log rather than
an investigation by elimination. That is the general lesson of both
entries: this class of defect is only found by driving a real client, so
what matters is how fast the next one can be read off the evidence.

The cost of getting this wrong is worth naming, because it is the reason
the rule above is written as a test: fabricating a row is a silent wrong
answer, the same failure mode P-82 fixed from the other direction (a real
table shadowed by the catch-all). Four relations that cannot be wrong —
one database that is the one open file, one role that is the one implicit
superuser, and a fixed table of parameters this server reports about
itself — are a bounded exception. A fifth relation added by analogy,
without the two-part test above, is how it stops being one.
