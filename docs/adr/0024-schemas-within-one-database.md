# ADR 0024: Schemas within one configured database

Date: 2026-09-12

Status: Accepted

## Context

The P-96 investigation found a roadmap ownership gap. The shipped server
opens one configured database, while table and index metadata use flat
maps keyed by exact local names. The wire layer reports fixed
`public` and `pg_catalog` namespace rows. Neither user-created
schemas nor SQL database creation had a roadmap owner.

The relevant implementation at this investigation was
`common::DbConfig::db_path`, `engine::Database::open_impl` and
`connect`, `catalog::CatalogState`'s name maps, and
`server::pg_catalog::answer_pg_namespace`. The engine's
`SessionContext` was shared and fixed; its Describe message carried no
session id. The wire `intercept_set_show_reset` acknowledged SET/RESET
without changing engine state. These are implementation boundaries future
namespace work must address, not features already supplied by a schema
qualifier.

`docs/adr/0023-sql-identifier-identity.md` settles the earlier SQL
boundary: quoted name identity and validation against fixed public.
It deliberately leaves future namespace placement open.
`docs/adr/0021-one-database-one-role-until-m22-and-m24.md` mentions
the missing database command, but previously treated the single
database row as a temporary implementation consequence rather than an
explicit scope decision.

The original P-96 recommendation assumed that real `pg_namespace`
queries require user-created schemas before M24. That does not follow.
PostgreSQL's [namespace catalog](https://www.postgresql.org/docs/15/catalog-pg-namespace.html)
records named namespaces and their ownership. Registered built-in
namespaces alone would provide real state for those rows. Whether users
may create more namespaces is a separate product decision.

## Decision

### One configured database per server instance

The server serves one database selected by its configured file path.
`CREATE DATABASE`, `DROP DATABASE`, SQL-driven file provisioning,
switching a connection between database files and cross-database queries
are outside the current roadmap. Separate configured server instances
are the deployment mechanism for separate databases.

This is a server scope boundary, not a prohibition on a library
application opening independent `Database` handles for different
files. `Database::connect` creates another session on its existing
engine. It does not select a different database.

M24's real `pg_database` relation continues to expose the one configured
database. Catalog-query support must not imply a cluster manager.
PostgreSQL distinguishes
[databases from schemas within them](https://www.postgresql.org/docs/15/manage-ag-overview.html);
supporting the latter does not require implementing the former.

A server that explicitly receives a different startup database name must
eventually reject it instead of silently attaching to the configured
database. M24.2 owns that consistency check alongside the real
`pg_database` answer: an omitted name selects the configured database,
an exact configured name succeeds, and another name gets
`3D000 invalid_catalog_name`. The current no-op startup handler does
not establish that check.

### User-created schemas belong inside M24

Retain the existing parent milestone order. Split M24 into sequential
sub-milestones:

- **M24.1 — Schemas with independent object names:** persistent namespace
  identity, schema creation and ownership, qualified object resolution
  and session search paths.
- **M24.2 — Catalog queries over registered objects:** ordinary queries
  over the resulting catalog state and retirement of wire text
  interception.

This follows the roadmap rule that a milestone includes its own
prerequisites. It does not insert or renumber a parent milestone.
M22 supplies authenticated roles and object-privilege machinery, and
M23.1 supplies the joins the catalog queries need; both precede M24.

M24.1 registers `public` and `pg_catalog` as built-in namespace
records with stable identity and ownership, and adds
`CREATE SCHEMA [IF NOT EXISTS] name [AUTHORIZATION role]` for user
namespaces. Embedded CREATE statements in CREATE SCHEMA are outside this
increment. Protect system namespaces from ordinary creation, ownership
changes and writes. Apply the existing role system to namespace
ownership, USAGE and CREATE checks; schema creation must not bypass
authorization simply because roles shipped earlier.

Relation identity becomes namespace identity plus the exact local name.
A table's column `Schema` type is still its tuple layout; it must not
be repurposed to mean an SQL namespace. Indexes belong to their table's
namespace. Namespace-qualified table/index lookup and uniqueness checks
must preserve exact quoted names from ADR 0023 and allow the same local
name in different namespaces. Object ids used by plans and catalog
references remain stable rather than being hashes of a name that can
later change.

Session `search_path` is engine state. Its SET/SHOW/RESET behavior,
`current_schema`, unqualified creation and lookup, parameter inference
and prepared-statement description must all use the requesting session's
effective path and privileges. Use public as the default user path.
Follow the PostgreSQL
[search-path rules](https://www.postgresql.org/docs/15/ddl-schemas.html)
for ordered lookup and the implicit system-catalog search. Preserve an
explicit pg_catalog position when supplied; implicit catalog lookup does
not make it the default user creation namespace. Define prepared-plan
revalidation when this resolution context changes before accepting
implementation tasks.

Until M24, ADR 0023's fixed-public resolution and temporary wire
user-table precedence remain the compatibility contract. M24 replaces
that policy with namespace lookup and retires its interception exception
when ordinary catalog queries become available. It must document the
resulting precedence change for unqualified names colliding with system
catalogs; explicit qualification always names the requested namespace.

### Durability and catalog consistency are part of namespace support

Namespace creation is transactional metadata work. Its rollback or crash
recovery must neither remove another transaction's committed objects nor
allow committed writes into a namespace whose creation can still be
undone. Establish transaction-owned protection and publication ordering
before adding new catalog write paths. The catalog's internal mutation
mutex is not a substitute for that protection; preserve
`docs/adr/0014-catalog-is-a-live-shared-object.md`'s prohibition on a
caller holding a catalog guard across a statement.

Before M24.1's first code task, record the persistent catalog layout,
bootstrap/recovery sequence, handling of existing files and namespace
locking/publication design in an implementation ADR. Existing objects
must retain their exact names and belong to public on upgrade. A format
change must use the versioned-header policy and an explicit upgrade or
rejection path; never reinterpret old catalog bytes silently.

M24.2 exposes the registered namespace and object identities through
`pg_namespace`, `pg_class` and their relationships. Built-in rows
created by catalog bootstrap are real catalog state. The single
configured database, role records and session settings must remain
available through the same ordinary-query interface when the wire
recognizers disappear. Do not lose a client's database node or scalar
configuration answers during that transition.

Schema removal follows the existing object-removal milestone M26:
`DROP SCHEMA [IF EXISTS] name RESTRICT` removes an empty user
namespace; nonempty and system namespaces are rejected. CASCADE and
ALTER SCHEMA are not included in the current roadmap. M28's logical
dump writes user schema definitions before their qualified objects and
data.

## Consequences

The roadmap now states what owns CREATE SCHEMA and what excludes CREATE
DATABASE. P-95 remains a bounded front-end correction that can ship
before the persistent namespace model; no namespace storage work is
pulled into that fix.

Namespace support is substantial future engine work, not the three
story points spent settling P-96's documentation. Its acceptance includes
same-named tables in two schemas, isolated session paths, authorized
access, prepared/direct agreement, exact introspection identities,
rollback and reopen, and both crash-injection sweeps when the storage
boundary changes. The namespace implementation is not being declared
ready by this planning ADR.

Revisit the database scope only for a concrete requirement to serve
multiple database files through one server endpoint, with an ADR covering
routing, file lifecycle, resource ownership and recovery. Adding a row
to `pg_database` or a grammar arm alone does not supply those things.

