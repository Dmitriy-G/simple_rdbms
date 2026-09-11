# ADR 0022: Session-context expressions are an allowlist, not a function registry

Date: 2026-09-11

Status: Accepted

## Context

A JDBC-family client sends `current_catalog` and `current_schema` during
connection setup. Until this decision they failed with
`42703 undefined_column`, because the binder resolves every bare name
against the current table's schema and `sql::Expr` had no variant for a
scalar expression that is neither a column, a literal nor an operator
(P-91). `SELECT current_schema` is not an exotic query; it is what a
driver asks before it will show a connection as usable.

The obvious small fix was to match the text in
`crates/server/src/pg_catalog.rs`, beside the `select version()` shape
already handled there. It fails for a structural reason rather than an
aesthetic one: an interception can only answer a statement it matches
*whole*. `select current_schema(), current_user` in one query, or
`WHERE name = current_schema` inside a larger statement, are invisible to
it, and `cli` — which never goes through the wire layer at all — could
never evaluate any of them.

The opposite over-reach was equally available: build a function
subsystem. A function catalog, resolution by name and argument types, an
evaluator, `now()`, `count(*)`. That is a milestone's worth of design
with its own questions (overloads, volatility, `NULL` handling,
aggregation state), and none of it is needed to answer the six names a
driver actually sends.

What made this worth an ADR is neither of those, but the boundary
between them. An allowlist with no stated limit grows one name at a time
until it is a badly-shaped function registry that nobody decided to
build.

## Decision

**A closed, explicitly enumerated set of session-context expressions is
recognized by the parser and resolved by the binder into a constant. It
is not a function mechanism, and it does not grow.**

- The set is `current_catalog`, `current_schema`, `current_user`,
  `session_user`, `user`, `current_database()` and `version()`
  (`sql::SessionContextName`). Spellings SQL writes as niladic *keywords*
  are valid bare; `current_database` and `version` are valid only in call
  position, because a user table may own a column with either name and a
  bare identifier must keep resolving to it.
- `planner::Binder` replaces each with a `Value` taken from a
  `SessionContext` built once when the engine opens: the database is
  `DbConfig::database_name()`, the schema is always `public`, the user is
  always `DbConfig::FIXED_USER_NAME`
  (`docs/adr/0021-one-database-one-role-until-m22-and-m24.md`). Below the
  binder nothing knows these existed.
- **Any other identifier in call position is `42883
  undefined_function`**, not a syntax error. `now()` reports what is
  missing rather than describing the text.
- **A general scalar-function subsystem is M20's** — the aggregation
  milestone, which needs argument-typed resolution and an evaluator for
  `count`/`sum`/`avg` regardless. M21's date and time functions consume
  that machinery rather than building a second one, and so does every
  user-visible function after it. Adding a name to this allowlist instead
  is the thing this ADR exists to forbid.

**A name the engine answers is never also matched as text by the wire
layer.** `select version()` was intercepted in
`crates/server/src/pg_catalog.rs` before this decision; the engine now
answers it, so the interception was deleted in the same change. Two
layers answering one question is two places to change and one of them
will be missed — and the interception's answer was the one that silently
won, since it ran first.

## Consequences

`SELECT current_catalog, current_schema, current_user` works in one
query, inside a `WHERE`, over the extended protocol and from `cli`, which
is the whole of what P-91 asked for. The result columns are named after
the expressions, because a driver reads them back by name.

Two things got worse, both narrowly and both on purpose. `select
version() as v` no longer works: the interception accepted a trailing
alias and the grammar has no select-list `AS` at all, so the loss is not
about `version()` but about every expression in every select list — it is
filed separately rather than papered over by keeping a second answering
layer alive. And an unqualified `user`, `current_user`, `session_user`,
`current_catalog` or `current_schema` now shadows a table column of that
name; PostgreSQL reserves these words for the same reason, and a
qualified `t.user` still resolves to the column.

The rule to apply when the next name arrives is the one this ADR is for:
if it is a *session* value — something the connection knows about itself
with no arguments and no computation — it may join the allowlist. If it
takes an argument, varies per row, or accumulates state, it is not a
session-context expression and it waits for M20's function subsystem. The
allowlist being short is the evidence that it is still an allowlist.
