# ADR 0007: Speak the PostgreSQL wire protocol, not Arrow Flight SQL or a bespoke driver

Date: 2026-08-25

Status: Accepted

## Context

`docs/ROADMAP.md`'s M14 gives this engine a network-facing SQL frontend,
after M10 (concurrent transactions) makes "isolation" a real guarantee
under concurrent connections rather than an accident of serial execution.
A network protocol is a much larger, harder-to-reverse commitment than
anything chosen so far in this project: clients, drivers, connection
poolers, and BI tools all have to speak it, and changing it later means
breaking every one of them at once. Three real options exist for what that
protocol should be:

1. **The PostgreSQL wire protocol**, implemented in Rust via the `pgwire`
   crate. Row-oriented, text/binary hybrid, designed for OLTP-shaped
   single-statement and simple-transaction workloads - the shape this
   engine already has.
2. **Arrow Flight SQL**, a gRPC-based protocol built around Arrow's
   columnar in-memory format, designed for bulk analytical result
   transfer between systems that already speak Arrow.
3. **A bespoke protocol and driver**, designed from scratch for exactly
   what this engine does today.

## Decision

Implement the PostgreSQL wire protocol via `pgwire`.

The deciding factor is client ecosystem, not protocol elegance: every
mainstream language has a mature, widely-used Postgres driver, every
connection pooler (pgbouncer, pgcat) and BI tool already speaks it, and
`psql` itself becomes a working client for this engine for free. A
from-scratch engine's whole value as a *learning* project (see the root
`README.md`) is in implementing the hard parts correctly - storage, WAL,
recovery, transactions - not in also asking every future user to adopt a
driver nobody has. Arrow Flight SQL was designed for a different shape of
workload: bulk columnar result transfer between analytical systems, where
gRPC's framing and Arrow's batch-oriented format pay for themselves. This
engine's row-at-a-time, Volcano-style executor (see
`crates/executor/README.md`) and OLTP-shaped statement mix (single-row
`INSERT`s, point/range `SELECT`s) is the workload the Postgres protocol
was built for, not the one Flight SQL was. A bespoke protocol was
rejected outright: it would require writing and maintaining a driver for
every client language this project might ever want, for a protocol
design problem that PostgreSQL already solved and that this project gains
nothing pedagogical from re-solving.

`pgwire` specifically, rather than hand-rolling the protocol: the wire
format itself (message framing, the extended query protocol's
parse/bind/execute/describe cycle, `COPY`, SSL negotiation) is large and
mostly mechanical - correctly implementing it by hand would be a
multi-milestone project of its own, orthogonal to everything M1–M13 were
actually about. `pgwire` is actively maintained, has no dependency on any
particular storage engine (unlike embedding a full Postgres-compatible
SQL layer), and exposes exactly the seam this project needs: a trait for
handling parsed queries and returning rows, which `crates/server` (or a
new frontend crate depending on it, per the workspace's dependency-edge
rules in `CLAUDE.md`) implements against `engine::Database`.

## Consequences

`sql`'s hand-written lexer/parser (see `crates/sql/README.md`) stays
exactly as it is - `pgwire` handles wire framing and the extended query
protocol's message flow, not SQL grammar, so this project's own parser
remains the one thing between wire-protocol input and a bound statement,
which is the whole point of having written it by hand in the first place.
The engine only needs to look like Postgres at the *wire* level, not
behaviorally: `docs/adr/0004-acid-scope.md`'s precise statement of what
ACID guarantees actually hold must stay true and visible to anyone
connecting a real Postgres client, since a driver author's assumptions
about Postgres semantics (isolation levels, error codes) will otherwise
be silently wrong against this engine. `common::SqlState` (see
`crates/common/src/sql_state.MD`) was built ahead of this exact need -
every error this engine can raise already carries a real SQLSTATE code,
so `pgwire`'s error responses have real data to report instead of a
generic failure. M10's concurrency work becomes a hard prerequisite, not
just a nice-to-have, the moment this ADR's frontend exists: multiple wire
connections mean multiple concurrent transactions for real, and
`txn::LockManager`/`txn::mvcc` (currently unwired scaffolding, per
`docs/adr/0004-acid-scope.md`) must be wired into every read and write
path before M14 can safely ship.
