# ADR 0023: SQL identifier identity and the public schema boundary

Date: 2026-09-12

Status: Accepted

## Context

The P-95 investigation found that the SQL front end rejected double
quotes and accepted only a bare table name. A client could obtain a
table's exact name from the catalog, then fail to read it using
`SELECT * FROM "public"."users"`. Unquoted identifiers retained their
original case, while catalog table, column and index names were stored
and compared exactly. Changing quoting therefore requires an identity
decision, not only another punctuation token.

This records the implementation contract; the identifier changes have
not shipped. It complements the one-database decision in
`docs/adr/0021-one-database-one-role-until-m22-and-m24.md` and the
session-expression rules in
`docs/adr/0022-session-context-expressions.md`. It does not decide the
roadmap placement of multiple schemas or databases.

## Decision

### Identity is established at the SQL boundary

Fold unquoted identifiers with ASCII lowercase conversion. Preserve the
decoded text of double-quoted identifiers exactly, including case,
Unicode, whitespace and punctuation; two adjacent double quotes encode
one literal double quote. Reject an empty quoted identifier, an embedded
NUL and an unterminated quote with a located SQL syntax error. A dot
inside quotes is part of the name.

This adopts PostgreSQL's distinction between quoted and unquoted names.
Its [identifier documentation](https://www.postgresql.org/docs/15/sql-syntax-lexical.html#SQL-SYNTAX-IDENTIFIERS)
defines the case and escaping rules. This engine continues to accept its
existing ASCII grammar for unquoted names; expanding that character set,
Unicode escape syntax and PostgreSQL's identifier-length truncation are
outside this change. Names are never silently truncated.

Retain quoting information in tokens until grammar decisions are made.
A quoted token is not a keyword or an optional `VERBOSE` modifier, and
a quoted table alias bypasses the bare-alias reserved-word check. A bare
`"current_user"` is a column reference, not a session expression.
In call position, match the existing session-function allowlist against
the normalized name exactly: `VERSION()` and `"version"()` can name the
same existing function, while `"VERSION"()` is undefined. Do not extend
the allowlist. Type-name syntax keeps its current unquoted spellings;
quoted type names are outside this increment.

Apply the same identity rule to table names, column names, index names,
table aliases and both components of an existing `table.column`
expression. Below the parser these components are ordinary normalized
strings. Catalog APIs accept exact names; they do not fold names or
interpret SQL quotes.

### Preserve qualification until the planner validates it

Represent a relation name in the SQL AST as
`QualifiedName { schema: Option<String>, name: String }`. Parse one or
two identifier components in every existing table position:
`SELECT ... FROM`, `INSERT INTO`, `CREATE TABLE`, and
`CREATE INDEX ... ON`, including their `EXPLAIN` forms. A third
component or a missing component is a syntax error.

Keep the index's own creation name unqualified. PostgreSQL's
[CREATE INDEX contract](https://www.postgresql.org/docs/15/sql-createindex.html)
also distinguishes the unqualified new index name from its optionally
qualified target table.

The planner owns the current namespace policy:

| Schema component | Ordinary engine behavior |
| --- | --- |
| Absent or exactly `public` | Resolve the exact local name in the existing catalog |
| Exactly `pg_catalog` | Return `0A000`: ordinary system-catalog execution is not implemented |
| Anything else, including quoted `"PUBLIC"` | Return `3F000 invalid_schema_name` |

These error codes use PostgreSQL's
[SQLSTATE names](https://www.postgresql.org/docs/15/errcodes-appendix.html);
the choice to reject an unknown schema explicitly in every table position
is this engine's contract, not a claim that every PostgreSQL statement
reports that same code.

Validate before catalog lookup or DDL allocation. Use one planner helper
from binding and prepared-parameter inference, including DDL and nested
`EXPLAIN` targets. The engine's describe path does not bind every
statement kind, so binder-only validation would leave a protocol gap.

After validation, the bound plan contains the existing local name or
table id. The persistent catalog format does not acquire a namespace
column. An unaliased `FROM public.t` exposes the range variable `t`;
an alias replaces it, just as it does today. This increment retains
existing one- and two-component column expressions; three-component
column expressions, qualified wildcards and select-list aliases are
separate grammar features.

`public` is the fixed namespace of ordinary engine tables. Session
settings do not create namespaces or change this resolution rule.
PostgreSQL's [schema search path](https://www.postgresql.org/docs/15/ddl-schemas.html)
is broader than this contract. Multiple schemas require a later catalog
design; qualification support alone does not implement them.

### Wire interception must respect exact relation identity

Keep the existing catalog-query recognizers until M24 replaces them.
Their routing must inspect the actual top-level `FROM` relation of a
`SELECT`, respecting strings, quoted identifiers, comments and
parentheses. Splitting normalized text on spaces is insufficient: a table
named `"x from pg_class y"` must never select the `pg_class` recognizer.

Preserve quoted spans and string literals during normalization and fold
only unquoted ASCII text. Decode the relation into separate components;
never split a decoded quoted name on its embedded dots. Use these routing
rules before any known-shape or catch-all answer:

- An explicit `public` or any schema other than exact `pg_catalog`
  always goes to the engine, including a missing `public.pg_class`.
- An unqualified name matching an actual catalog table exactly goes to
  the engine. Preserve this existing user-table precedence, even for a
  name such as `pg_class`; this is not PostgreSQL search-path emulation.
- Otherwise, only an unqualified or explicitly `pg_catalog`-qualified
  canonical lowercase ASCII `pg_` name is eligible for interception.
  Both `pg_catalog."pg_class"` and `"pg_catalog"."pg_class"` are
  eligible; `"PG_CLASS"` and the single identifier
  `"pg_catalog.pg_class"` are not.
- Malformed or unclassifiable relation syntax is declined and goes to the
  engine. Never discard an unknown schema or an extra name component.

Pass the parsed relation to every recognizer, the catch-all and the
shadow check. The shared `answer` path governs simple query, extended
Parse and the existing recheck at Execute. Preserve cached-plan
invalidation when an ordinary table appears after an introspection
statement was prepared.

This is a correction to relation routing within the existing interceptor,
not a general catalog-query evaluator. Its projection and predicate
shape limitations remain. Within its existing equality-string filters,
decode doubled single quotes so a stored name containing an apostrophe
can be looked up exactly. Metadata names remain exact strings, never SQL
fragments; the client quotes each component and doubles embedded quotes.

### Existing database files retain their identities

Do not rename, merge, lowercase or rewrite stored objects on open.
There is no format-version bump and no compatibility fallback lookup.

For new SQL after this change, `CREATE TABLE Users` creates `users`;
`CREATE TABLE "Users"` creates a distinct `Users`. A pre-change stored
`Users` remains accessible as `"Users"`, and its stored column names
need the same exact quoting. If both stored spellings already exist,
unquoted `Users` resolves to `users` after the change. Applications
using legacy mixed-case SQL must therefore quote the intended stored
names; this can change which existing object a statement selects, not
only turn a success into an error.

A reopen regression uses exact-name catalog creation to construct the
legacy mixed-case state, then exercises it through SQL. That API has the
same persisted representation and avoids committing a binary database
fixture or retaining the old lexer as a compatibility mode.

## Consequences

A client can round-trip an ordinary table's catalog name through
component-wise quoting, and prepared statements use the same qualified
name as direct execution. Literal values remain redacted in fingerprints;
the original spelling of identifiers is retained in log source spans.

The implementation spans SQL tokens/AST/parser, planner resolution and
parameter inference, common error mapping, engine integration tests and
wire interception. It changes public AST and error APIs, but requires no
storage algorithm, WAL format, locking or persistent catalog change.

Future namespace work must replace the fixed planner policy with real
namespace lookup before admitting another schema. M24 must replace the
wire recognizers while preserving exact name identity. Broader SQL
grammar, full catalog-query semantics and the placement of those
milestones are not prerequisites for this bounded correction.

