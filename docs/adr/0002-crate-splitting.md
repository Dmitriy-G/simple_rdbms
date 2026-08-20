# ADR 0002: Split the engine into layered crates

Date: 2026-08-20

Status: Accepted

## Context

A database engine has well-known layers — storage, catalog, SQL front end,
transactions, query planning, execution — that a from-scratch
implementation could still put in one crate with internal module
boundaries. We need to decide whether module-level or crate-level
boundaries better serve this project.

## Decision

Each layer is its own crate (`common`, `types`, `storage`, `catalog`,
`sql`, `txn`, `planner`, `executor`, `engine`, `cli`), with a strict,
explicitly enumerated set of allowed dependency edges between them (see
`CLAUDE.md`). No crate may depend on anything not on that list, and there
are no cycles.

We chose crates over modules for one reason: `cargo` enforces crate
dependency edges as a hard compiler error, while module visibility
(`pub(crate)`, `pub(super)`) is easy to route around from inside the same
crate under time pressure. Given this project's goal is to *learn* the
layering of a database engine correctly, making a layering violation
impossible to compile is more valuable than the extra `Cargo.toml`
boilerplate it costs.

## Consequences

- Compilation is the enforcement mechanism for the architecture: a PR that
  makes `sql` depend on `executor`, for instance, fails to build, not just
  fails review.
- Each crate compiles (and its tests run) independently, which keeps
  iteration fast as the workspace grows and makes each layer's public API
  explicit by construction — only `pub` items in a crate's root are
  visible to dependents at all.
- The cost is more ceremony per layer: every crate needs its own
  `Cargo.toml`, its own error type that converts into `common::Error`, and
  its own smoke test. We accept this cost once, up front, in exchange for
  the dependency graph staying legible as the codebase grows.
- `common` and `types` sit below everything and must stay free of any
  dependency on a sibling crate, or the whole point of the split is lost.
