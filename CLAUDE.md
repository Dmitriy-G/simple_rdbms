# CLAUDE.md

Guidance for anyone — human or agent — working in this repository.

## What this is

A relational database engine built from scratch in Rust, as a learning
project structured as a layered Cargo workspace (`docs/adr/0002-crate-splitting.md`).
What works today: durable slotted-page storage with checksummed pages, a
buffer pool with LRU-K eviction, write-ahead logging, ARIES-style crash
recovery with fuzzy checkpointing, a double-write buffer protecting
against torn page writes, `BEGIN`/`COMMIT`/`ROLLBACK` transactions, a
B+tree index the optimizer picks over a sequential scan automatically
(`planner::optimizer::IndexScanRule`), `EXPLAIN` for both logical and
physical plans, a Prometheus metrics and liveness/readiness server, and a
`Send + Sync` storage layer built for a future multi-connection frontend.
What does not exist yet: `DELETE`/`UPDATE`, multi-table joins, column
constraints (`NOT NULL`/`UNIQUE`/`FOREIGN KEY`), a network wire protocol,
and concurrent statement execution. See `docs/ROADMAP.md` for exactly
what closes each of those gaps and in what order.

## Contents

- [Where to read next](#where-to-read-next)
- [Working from task.MD](#working-from-taskmd)
- [LLM roles and channels](#llm-roles-and-channels)
- [Invariants that must not be broken](#invariants-that-must-not-be-broken)
- [Known scaffolding](#known-scaffolding)
- [Commands](#commands)
- [Dependency-edge rules](#dependency-edge-rules)
- [Documentation](#documentation)
- [Lint suppressions](#lint-suppressions)
- [Error handling](#error-handling)
- [Logging](#logging)
- [Commit and branch conventions](#commit-and-branch-conventions)
- [Testing rules](#testing-rules)

## Where to read next

A fresh session needs to know what to open before it needs to know how to
name a branch:

1. `docs/ROADMAP.md` — every milestone, what's done, what's next, and the
   dependency that fixes each one's position. Milestone numbers are
   permanent identifiers: a completed milestone keeps its number forever,
   and implementation order has twice diverged from numeric order without
   renumbering anything (M12 shipped ahead of M9; M14 no longer depends
   on M10 — see the roadmap's own introduction for why).
2. `docs/adr/` — the ADRs recording decisions that are expensive to
   rediscover. The ones a change is most likely to violate without
   reading first: 0003 (index after the log), 0004 (what ACID means
   here today), 0005 (the durability boundary after the double-write
   buffer), 0008 (write-guard reentrancy), 0009 (buffer pool frame
   ownership), 0010 (waiting for a frame instead of failing).
3. The relevant `crates/<crate>/README.md`, then the sibling `.MD` of
   each file being changed.

## Working from task.MD

When asked to **"do next task"**, follow this procedure:

1. Read `.claude/task.MD`. If it contains a numbered
   subtasks list (1 to N), work through that list in the order given —
   that is the "recommended order." Complete one subtask, then stop and
   hand control back for review before starting the next one. If
   `.claude/task.MD` has no subtasks list, treat the whole file as a single task
   and do it in full.
2. `.claude/problems.MD` is where incidental discoveries
   go: if, while working a subtask, you notice a problem that is real but
   does not depend on or belong to the subtask in progress, record it in
   `.claude/problems.MD` rather than investigating or fixing it there. Fixing it
   is a separate, later task.
3. Do not go beyond the subtask boundary implied by the order list — a
   subtask is done when its own scope is satisfied, not when adjacent
   related work is also finished.

## LLM roles and channels

This repository is worked through six LLM roles, one file per role in
`.claude/agents/`. Those files are authoritative for what each role may
write; this section is the overview, and
`docs/diagrams/README.md` indexes a flow diagram for each way work moves
between the roles. Determine the role
from the question being asked — a single session can switch roles
between questions — and state the active role at the top of every reply
about this project:

```
Role: <role name>

<response text>
```

### Roles

1. **Coder** — asked to work `.claude/task.MD`. Do the tasks it lists, in the
   order its Order Plan gives, and fix bugs listed in `.claude/bugs.MD`. Never
   commit automatically. Don't install heavy tooling (e.g. Python/pip)
   for investigating — use `bash` instead. If a problem surfaces that
   isn't part of the current subtask, record it in `.claude/problems.MD` rather
   than investigating or fixing it there. If a needed investigation is
   itself large (e.g. testing a hypothesis), ask before doing it — that
   is usually Architect's work, not Coder's. `.claude/task.MD` typically holds
   several subtasks or bugs tied to one milestone; work through them one
   at a time and stop after each one so it can be reviewed before the
   next starts.
2. **Architect** — asked to investigate an entry from `.claude/problems.MD` or
   review the project as a whole (documentation, module structure,
   etc.). Record findings in `.claude/investigations.MD`. Owns the
   project's cross-cutting prose and its process: `docs/adr/**`, the
   roadmap's entry text (never its status markers), this file, crate
   `README.md`s, and `.claude/agents/*.md` plus `.claude/settings*.json`.
   Never touches a `.rs` file, a test, or a sibling module `.MD`.
3. **Task writer** — asked to turn a user request into a task for the
   Coder role. Write `.claude/task.MD` using the task format below: keep it
   understandable but short — the Coder role doesn't need root causes or
   other background, just the task. Decompose a large task or
   sub-milestone into several subtasks and order them with an Order
   Plan. Archives the finished `.claude/task.MD` to `docs/tasks/` and
   sets 🚧 In Progress in `docs/ROADMAP.md`.
4. **Code Reviewer** — asked to do a code review of one subtask's
   change. Check the code against general conventions for the tech stack
   and against this file's project-specific rules, and confirm it
   actually implements what `.claude/task.MD` described. Record anything
   wrong in `.claude/bugs.MD` using the bug format below, with concrete
   instructions on how to fix it.
5. **Milestone Reviewer** — asked to review a finished milestone as a
   whole against its `docs/ROADMAP.md` entry, once every subtask has
   passed code review: the milestone's Done-when, cross-cutting
   invariants, documentation truth, forward dependencies it created, and
   deferred items. Writes `.claude/bugs.MD` and `.claude/problems.MD`,
   and is the only role that sets ✅ Done in `docs/ROADMAP.md`.
6. **Helper** — the default role: anything not covered by the five roles
   above, such as answering a question about the project. Read-only.

### Channels

Four files under `.claude/` carry the roles' communication. Each has one
primary writer; the extra writers listed are deliberate:

- Tasks channel — `.claude/task.MD`. Written by Task writer (and by
  Architect only when the human explicitly asks). Read by Coder and both
  reviewers.
- Problems channel — `.claude/problems.MD`. Written by Coder; also by
  Milestone Reviewer for non-defect findings, and by Architect for
  status lines and for problems an investigation uncovers. Read by
  Architect.
- Investigations channel — `.claude/investigations.MD`. Written by
  Architect. Read by Task writer and by the human.
- Bugs channel — `.claude/bugs.MD`. Written by Code Reviewer and
  Milestone Reviewer. Read by Coder.

No role commits. Finished work is left in the working tree for the human
to review and commit.

### File formats

`.claude/task.MD`:
- Title: milestone number + a short description.
- Order Plan: a numbered list (1 to N) giving the subtask order.
- A description for every subtask in the Order Plan, including how to
  test it.

`.claude/problems.MD`, per entry:
- Title: problem number + a short description.
- Description: full detail.

`.claude/bugs.MD`, per entry:
- Title: bug number + a short description.
- Reason: why it's a problem, in short.
- Description: full detail.
- How to prevent in future: a concrete instruction — a lint, a test,
  etc.

## Invariants that must not be broken

This is the most important section in this file. Each of these is
currently documented in full only in one `.MD`, so a change made without
opening that file first can break it without any test catching the
mistake at review time.

- **Log before page.** A dirty page may never reach disk before the log
  record describing it. `storage::buffer::BufferPool::flush_pages` forces
  the log durable to the batch's highest `page_lsn` before writing any
  page to its real location (`buffer.MD`), and `PageWriteGuard::write` is
  the only way ordinary code mutates a page's bytes — never
  `Page::data_mut()` directly. The one documented exception is
  `BufferPool::stamp_write`, used only by `recovery::recover` to reapply
  a change that is already durably logged (Redo) or already covered by
  its own `Clr` record (Undo), where logging again would misdescribe
  what happened.
- **Latch ordering.** Take the buffer pool's index lock, find or install
  the frame, pin it, release the index lock, then take the frame's own
  latch. Never hold the index lock across a frame latch or across disk
  I/O. The one documented exception is `try_install`, which holds both
  briefly, but only for a frame not yet reachable through the page table
  (`buffer.MD`'s latch-ordering rule).
- **One write guard per page per thread.** `RwLock` is not reentrant; a
  second write guard on a page this thread already holds one for
  self-deadlocks. Use `fetch_page_read` for a second, read-only view.
  Debug builds detect the mistake and panic instead of hanging
  (`docs/adr/0008-write-guard-reentrancy.md`).
- **All-zero pages are valid.** The `NO_NEXT_PAGE` constant in
  `crates/storage/src/heap.rs` is `PageId(0)`, and a slotted page's
  used-space count is computed so it reads as zero on an untouched page,
  specifically so a freshly allocated page — all zero bytes, never
  explicitly initialized — decodes as a valid, empty page rather than a
  corrupt one. This came from a real bug: a page allocated but never
  flushed read back as all zeros, its zero `next_page_id` was taken for a
  real page id, the heap scan followed it to page 0, and the file
  header's magic bytes were read as a slot count of 17734, walking off
  the end of the page. See `heap.MD`'s `NO_NEXT_PAGE` entry for the fix
  this motivated. Any new on-disk struct must preserve the same property:
  pick encodings where all-zeros is a legal, safe state.
- **The page-0 header is versioned and logged.** Its format version is
  checked on open (`disk::header::VERSION_RANGE`) and every mutation to
  it goes through `PageWriteGuard::write` like any other page, so it is
  redone and undone exactly like the rest of the database rather than
  racing ahead of it as an unlogged direct write.
- **Errors are logged once, at the engine boundary.** See the "Error
  handling" section below rather than duplicating it here.

## Known scaffolding

Code that exists but cannot be reached yet, so it should not be mistaken
for working capability. Each is named with the milestone that finishes
it — this was requested in an earlier roadmap task and never landed, and
belongs here rather than in `docs/ROADMAP.md` since this is the file a
fresh session reads first:

- `catalog::Column::nullable` — parsed, persisted, plumbed through the
  binder, never enforced (M16).
- `common::SqlState::NOT_NULL_VIOLATION` — defined, never raised (M16).
- `common::SqlState::UNIQUE_VIOLATION` — defined, never raised (M17); the
  B+tree still permits duplicate keys.
- `storage::btree::BTreeIndex::delete` — the method exists; its body is
  `todo!()` (M15).
- `executor::NestedLoopJoinExecutor` — exists and is wired into the
  executor factory; `init` and `next` are both `todo!()` (M11).
  `planner::LogicalPlan::Join`/`PhysicalPlan::NestedLoopJoin` already
  exist as the node kinds it would run, but nothing in `sql`'s grammar
  can produce them yet — `FROM` accepts exactly one table (M11).

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

Check documentation:
```sh
bash scripts/check_docs.sh
```

All five (`fmt --check`, `clippy -D warnings`, `build`, `test`,
`check_docs.sh`) must pass on a clean checkout; CI
(`.github/workflows/ci.yml`) runs the same commands on every PR.

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

`test-support -> common, storage` is the one exception to "no edge beyond
this list," and it is a `dev-dependency` everywhere it appears rather than
a normal dependency: `storage`, `catalog`, `executor`, `txn`, and `engine`
each add it under `[dev-dependencies]` so their own `tests/` integration
suites can share one copy of test-only fixtures (`crates/test-support/README.md`)
instead of the twelve-plus copies `crates/test-support/src/lib.MD` replaced.
Being dev-dependency-only means it never reaches a shipped binary and
never participates in the normal-dependency cycle check the compiler
already enforces for the table above, so it does not need its own row in
that table — this paragraph is its edge instead.

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

## Lint suppressions

Lint suppressions live in configuration, not in source. A lint that needs
disabling is disabled in the workspace `Cargo.toml`'s
`[workspace.lints.clippy]`, in a crate's own `[lints.clippy]` table, or in
`clippy.toml` — never as an `#[allow]` or `#[expect]` attribute in a `.rs`
file. This follows from the no-comments convention above: code stays
clean and the prose explaining a suppression lives elsewhere, not next to
it.

The current exceptions are a known, closed set - six `#[allow(dead_code)]`
attributes across four files, none of them expressible in configuration
since each silences one specific field or item rather than a lint
crate-wide:

- `crates/txn/src/lock_manager.rs`'s `LockManager::holders` and
  `crates/executor/src/operators/nested_loop_join.rs`'s
  `NestedLoopJoinExecutor::{left, right, predicate}` mark work that
  belongs to a milestone not yet built (M10's lock manager, M11's join
  executor, `docs/ROADMAP.md`) and should disappear when that milestone
  lands and starts reading them.
- `crates/storage/src/disk.rs`'s `DiskManager::path` and
  `crates/storage/src/replacer.rs`'s `LruKReplacer::capacity` are fields
  kept for future use that nothing reads yet.

Do not add a seventh without updating this list.

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

Integration tests in `tests/` remain the default and the only place
anything touching I/O, locks, the buffer pool or the log may be tested. A
`#[cfg(test)] mod tests` block in a `src/` file is permitted **only** for
private functions that are pure: no I/O, no locking, no shared state,
arguments in and a value out. If a test needs a `BufferPool`, a
`DiskManager`, a device or a temp directory, it belongs in `tests/`.
Everything else, including a test that merely happens to sit near the
code it exercises, goes in `tests/`, driven through the crate's public
API: `#[cfg(test)]` only takes effect when compiling a crate's own
unit-test binary, so anything gated behind it is invisible to `tests/`,
and a binary-only crate has no `tests/`-reachable target at all unless it
also ships a `[lib]`. When a test helper needs to be shared across
multiple files under `tests/`, it lives in `tests/support/`, declared
with `mod support;` by whichever test file needs it — never behind
`#[cfg(test)]` in `src/`, which would hide it from every other test file
that wants it too.

A test may assert on log output only when logging is the behavior under
test — `crates/engine/tests/logging.rs` and
`crates/storage/tests/recovery_logging.rs` are the legitimate case, since
each is checking that a specific event actually gets logged, with the
right fields, when it should be. Using a log line as a *proxy* for some
other behavior is not legitimate: a durability or correctness invariant
must be asserted through the public API, a counter, or on-disk state, not
by grepping captured tracing output for a message string that carries no
contract not to be reworded. `crates/engine/tests/sessions.rs`'s
`several_sessions_checkpoint_once_per_threshold_not_once_each` used to
violate this - counting `"checkpoint complete"` events to prove
checkpoints don't multiply with sessions - and a real counter
(`engine::Database::stats()`, gated `#[cfg(any(test, feature =
"test-util"))]`) replaced it.

The crash-injection harness (`crates/engine/tests/crash_injection.rs`,
`crates/storage/tests/btree_crash_injection.rs`) is the repository's
primary correctness gate for durability, and sweeps every write point
across several workloads under four durability models
(`DurabilityModel::write_is_durable`/`requires_sync`/`torn_write`/
`torn_write_requires_sync`, `crates/storage/src/block_device.rs`),
asserting the
state recovered after a crash at that point matches a safely committed
prefix. Any change to storage, the WAL, recovery, or the double-write
buffer must run both of these before it is considered done, and a change
that alters their results is wrong until proven otherwise.

Concurrency tests carry their own discipline. Bound every repetition with
a timeout — a hang and a slow pass are otherwise indistinguishable
without one — and reproduce CI's low-core scheduling with `taskset -c
0,1` (or an equivalent CPU-affinity constraint) rather than trusting a
wide local machine; a race that needs real contention to manifest can
pass every time on a many-core workstation and still fail reliably in
CI.
