# CLAUDE.md

Guidance for anyone — human or agent — working in this repository.

## Commands

Build:
```sh
cargo build
```

Run the REPL:
```sh
cargo run -p cli -- [db_path]
```

Format:
```sh
cargo fmt --all              # apply
cargo fmt --all -- --check   # verify, as CI does
```

Lint (must be warning-free):
```sh
cargo clippy --workspace --all-targets -- -D warnings
```

Test:
```sh
cargo test --workspace
```

All four (`fmt --check`, `clippy -D warnings`, `build`, `test`) must pass
on a clean checkout; CI (`.github/workflows/ci.yml`) runs the same commands
on every PR.

## Dependency-edge rules

The workspace is split into layered crates under `crates/`, and the
allowed dependency edges between them are fixed:

```
common   -> (none)
types    -> common
storage  -> common, types
catalog  -> common, types, storage
sql      -> common, types
txn      -> common, storage
planner  -> common, types, catalog, sql
executor -> common, types, catalog, storage, txn, planner
engine   -> all of the above
cli      -> engine, common
server   -> engine, common
```

No cycles, no back-edges, and no edge beyond this list. This is enforced
by the compiler (a disallowed `path`/`workspace` dependency simply won't
build), not by convention — see `docs/adr/0002-crate-splitting.md` for why.
If a crate seems to need a dependency not on this list, that's a sign the
change belongs in a different crate, or the edge list itself needs an ADR
updating it — don't just add the dependency.

## Documentation

No comments in code. Every `crates/**/*.rs` file has a sibling `<stem>.MD`
beside it in the same directory (`buffer.rs` -> `buffer.MD`,
`tests/wal.rs` -> `tests/wal.MD`), uppercase extension, test files
included. Any new `.rs` file must ship with its `.MD` in the same commit.

The `.MD` file mirrors this structure exactly:

```
# <the .rs file's stem>

<Opening paragraph: what this module does, where it sits in the
pipeline, and the design rationale behind it. Link ADRs as
docs/adr/NNNN-slug.md and cross-reference sibling modules by filename.>

## Key Components

- `TypeName` - What it represents and the invariant it maintains.
- `TypeName::method(args) -> Ret` - What it does, when it's called,
  and the edge cases or ordering constraints that matter.
- `free_function(args) -> Ret` - Same.
- `private_helper(args)` - Private. Only listed when it carries real
  logic rather than being a one-line accessor.

## Usage Example

<a short, realistic call sequence showing how this module is driven
in context - illustrative, not a compiling doctest, in a fenced
```rust block>

<Closing paragraph: how this is actually invoked by its callers and
what guarantee that ordering or sequencing buys.>
```

This is a migration, not a deletion: no reasoning may be lost when a
comment moves into the `.MD`. Two exceptions stay in the `.rs` file itself
rather than moving: `// SAFETY:` comments on `unsafe` blocks (required by
clippy's `undocumented_unsafe_blocks`) and `// TODO(Mx):` milestone
markers. Every other `///`, `//!`, and ordinary `//` comment is disallowed.

Every crate under `crates/` also carries a `crates/<crate>/README.md`
(lowercase, distinct from the uppercase `.MD` module docs) with a fixed
outline: a one/two-sentence role statement, then `## Architecture` (where
the crate sits in the layered workspace and why), `## Key Components` (one
bullet per module, linking to its `.MD`), `## Features` (what works today,
what's deliberately stubbed, referencing `docs/ROADMAP.md` milestones by
number), `## Dependencies` (workspace edges plus external crates and why),
`## Configuration` (knobs and defaults, or a one-line "none"), and
`## Testing` (where tests live and how to run just this crate). It is the
outside view of the crate - why it exists, what it offers - while
`src/lib.MD` documents the crate root module itself; the two must not
duplicate content, only cross-link. A new crate ships with its README in
the same commit that adds it, same as a new `.rs` file ships with its
`.MD`.

`scripts/check_docs.sh` enforces all of this in CI: missing `.MD`
siblings, missing `.rs` siblings, a missing `## Key Components` or
`## Usage Example` heading, an `.MD` file's title not matching its stem, a
public item undocumented in its sibling `.MD`, a crate missing its
`README.md`, or a disallowed comment all fail the build.

## Error handling

`common::Error` is the one error type every layer converges on; every
variant is named after the SQL condition it represents (`UndefinedTable`,
not `Catalog`) and carries structured fields rather than a formatted
string, and `Error::sql_state()`/`Error::severity()`/`Error::is_retryable()`
give it machine-readable identity (a SQLSTATE code, a wire-protocol
severity, and whether a client should retry) without any server or client
protocol existing yet. See `crates/common/src/error.MD` and
`crates/common/src/sql_state.MD` for the full design and the exhaustive
variant-to-code mapping.

Errors are logged exactly once, at the boundary where they leave the
engine - `engine::Database::open` for startup failures,
`engine::Database::execute` for statement failures - and nowhere else.
Every intermediate `?`/`From` conversion between layers must stay silent;
logging at each layer a failure passes through produces the same failure
reported five times at five levels of detail instead of once with full
context. If a new fallible boundary is added above `engine`, it inherits
this rule: log there, and only there.

## Logging

Structured logging via `tracing`, built for a container from the start:
JSON records, no log files, no file paths in config. A binary's own
subscriber decides where those records go; libraries never do — every
crate below a binary only ever calls `tracing::{trace,debug,info,warn,error}!`
or `#[tracing::instrument]`, never constructs a subscriber. Today that
subscriber is initialized exactly once, in `cli::init_logging`, at `info`
by default and controlled by `RUST_LOG`
(`crates/cli/src/main.MD`); a future server binary initializes its own the
same way. `cli` writes to **stderr**, not stdout — see
`crates/cli/src/main.MD` for why: stdout there is an existing, tested REPL
contract, not the general rule's target. A future headless server binary
writes JSON to stdout only, exactly as the rule says, since it has no
competing use for that stream.

Three nested spans carry correlation automatically to every event inside
them, so no lower layer has to thread ids through by hand: a connection
span (`conn_id`, opened once per `Database`, entered for its whole
lifetime), a transaction span (`txn_id`, `isolation`, entered around every
statement that joins an explicit transaction), and a statement span
(`stmt_id`, `statement_kind`, one per `Database::execute` call). See
`crates/engine/src/database.MD` for exactly how they nest and the one
case (autocommit) where a transaction's own fields end up nested under
its statement's span instead of the reverse.

**Never log user data above `DEBUG`.** A database log is a
data-exfiltration path. At `info`, a statement is logged as a normalized
fingerprint with literals replaced by `?` (`sql::fingerprint` — see
`crates/sql/src/fingerprint.MD`), never the raw text. Full SQL text and
the literal values it contains go at `debug` and no higher — the same
rule, since a literal in the fingerprinted-away spot is already visible
in the raw text at that level. Row *data* — anything read back from a
table, a `Vec<Tuple>` a query produced — is different: it never gets
logged at any level, `trace` included, full stop; there is no debugging
need `?rows` is worth the exfiltration risk for.

What to log, by level: `error` — recovery failure, checksum mismatch,
unrecoverable I/O, any `Severity::Fatal`. `warn` — transaction abort, a
double-write-buffer restore during recovery (the last shutdown was
unclean), a statement past `DbConfig::slow_query_warn_threshold_ms`,
buffer-pool eviction pressure (every frame pinned). `info` — startup and
shutdown, connection open/close, the recovery summary (records scanned,
winners, losers, duration), checkpoint completion with its LSN, and a
statement's fingerprint once it succeeds. `debug` — a statement's full SQL
and the physical plan chosen for it. `trace` — page fetch/evict, WAL
record append, page flush. The startup line (`engine::Database::open_with_managers`)
is the most valuable one in the whole system: it records the durability
configuration — page size, buffer pool frames, DWB capacity, whether
checksums are on — and the recovery outcome, so a later question of
whether a lost write was possible has a one-line answer.

Hot-path discipline: `trace!` inside `BufferPool::fetch_page` and
`LogManager::append` is fine — a disabled level compiles down to a cheap
check — but nothing on those paths may build a `String` or call `format!`
outside the macro, and no page pin or latch may be held across a logging
call.

## Commit and branch conventions

Commits follow [Conventional Commits](https://www.conventionalcommits.org/):

```
<type>(<scope>): <summary>
```

`type` is one of `feat`, `fix`, `refactor`, `test`, `docs`, `chore`,
`perf`, `build`, `ci`. `scope` is normally a crate name (`storage`,
`planner`, ...) or `workspace` for changes spanning crates.

Branches are named `<type>/<kebab-description>`, e.g.
`feat/buffer-pool-eviction` or `fix/slotted-page-overflow`.

## Testing rules

Tests must be deterministic: no assertions that depend on wall-clock time,
hash map/set iteration order, or thread scheduling. Use `tempfile` for
anything touching the filesystem so tests don't collide or leave state
behind, and seed any randomness (`proptest` included) rather than relying
on ambient entropy.

An inline `#[cfg(test)] mod tests` in `src/` is justified only when the
test needs access to a private item that should stay private — nothing
else earns the gate. Everything else, including a test that merely
happens to sit near the code it exercises, goes in `tests/`, driven
through the crate's public API: `#[cfg(test)]` only takes effect when
compiling a crate's own unit-test binary, so anything gated behind it is
invisible to `tests/`, and a binary-only crate has no `tests/`-reachable
target at all unless it also ships a `[lib]`. When a test helper needs to
be shared across multiple files under `tests/`, it lives in
`tests/support/`, declared with `mod support;` by whichever test file
needs it — never behind `#[cfg(test)]` in `src/`, which would hide it from
every other test file that wants it too.
