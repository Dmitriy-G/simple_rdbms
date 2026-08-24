# ADR 0006: Document crates with sibling `.MD` files, not rustdoc

Date: 2026-08-24

Status: Accepted

## Context

Every crate in the workspace has now been converted (`common`, `types`,
`storage`, `catalog`, `sql`, `txn`, `planner`, `executor`, `engine`,
`cli`): each `crates/**/*.rs` file, source and test alike, has gained a
sibling `<stem>.MD` file in the same directory, and every `///`, `//!`,
and ordinary `//` comment that isn't `// SAFETY:` or `// TODO(Mx):` has
been stripped from the `.rs` files. `scripts/check_docs.sh` enforces both
halves of this in CI: every `.rs` has a paired `.MD` (and vice versa),
every `.MD` has a `## Key Components` heading, and no disallowed comment
survives in code. This ADR records that decision now that it is fully
applied, rather than leaving the convention implicit in `CLAUDE.md` and a
run of commit messages.

The alternative was the standard one: `///`/`//!` doc comments compiled
by `cargo doc` into an HTML API reference, with fenced ```rust blocks run
as doctests by `cargo test`. That is what most of this workspace's
history used before this convention, and it is what most Rust crates use.
This project deliberately moved away from it.

## Decision

Documentation for a Rust source file lives in a sibling file with the
same stem and an uppercase `.MD` extension, never in `///`/`//!` comments
in the `.rs` file itself. Each `.MD` follows a fixed shape: an opening
paragraph describing what the module does, where it sits in the
pipeline, and the design rationale behind it; a `## Key Components`
section with one bullet per public type, trait, function, and any
private helper that carries real logic; a `## Usage Example` showing how
the module is actually driven by its real callers in this repository,
not an invented plausible-looking snippet; and a closing paragraph on how
callers actually invoke it and what guarantee that sequencing buys.

This is a deliberate trade, made for two honest reasons rather than a
technical necessity: consistency with the author's other projects, which
use the same sibling-file convention, and keeping prose out of the `.rs`
files so a source file reads as pure code and the reasoning behind it
lives in one predictable place instead of interleaved with it.

Two categories of comment are the deliberate exception and stay in the
`.rs` file: `// SAFETY:` comments on `unsafe` blocks, because clippy's
`undocumented_unsafe_blocks` lint requires them at the exact call site it
checks, and `// TODO(Mx):` milestone markers, which track in-progress
work against a specific milestone rather than documenting finished
design.

## Consequences

- `cargo doc` now produces an effectively empty API reference: with no
  `///`/`//!` comments left anywhere in `crates/`, every generated page
  lists signatures with no accompanying prose. `cargo doc` is not part of
  this project's toolchain or CI pipeline as a result: the `.MD` files are
  the documentation, read directly or rendered as plain Markdown, not
  through `rustdoc`.
- Doc examples are no longer compiled or run by `cargo test`. The
  `## Usage Example` block in each `.MD` is illustrative Markdown, not a
  compiling doctest, and nothing enforces that it stays in sync with the
  code the way a real doctest would. This traded away automatic
  verification of every example for the freedom to show a realistic call
  sequence (including one spanning multiple types across a real caller,
  which a single doctest often can't express cleanly) rather than a
  minimal one contrived to compile in isolation. One `compile_fail`
  doctest survived in `storage/src/page.rs` past the storage crate's own
  conversion commit, demonstrating that `BufferPool` exposes no manual
  unpin method; it was removed in this finishing pass once confirmed that
  `page.MD`'s `PageGuard` entry already carried the same point in prose.
- `scripts/check_docs.sh`, run in CI's "Check docs convention" step,
  is the actual enforcement mechanism, not reviewer discipline: a PR that
  adds a new `.rs` file without its `.MD`, or that reintroduces a `///`
  comment, fails the build the same way an unformatted file or a clippy
  warning does.
- New `.rs` files must ship their `.MD` sibling in the same commit
  (`CLAUDE.md` already states this); this ADR does not change that
  requirement, only records why it exists.
