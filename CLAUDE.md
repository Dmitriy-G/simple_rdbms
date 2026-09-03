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
- [Working from task.md](#working-from-taskmd)
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

1. `docs/ROADMAP.md` — every milestone, what's done, and what's next, in
   priority order. The number *is* the priority: they run 1, 2, 3 with no
   gaps, each milestone is a self-contained unit of work, and removing or
   inserting one renumbers everything after it. A number is therefore a
   position, not a name — so a milestone identifier written down anywhere
   else in the tree moves when the roadmap does, in the same change.
2. `docs/adr/` — the ADRs recording decisions that are expensive to
   rediscover. The ones a change is most likely to violate without
   reading first: 0003 (index after the log), 0004 (what ACID means
   here today), 0005 (the durability boundary after the double-write
   buffer), 0008 (write-guard reentrancy), 0009 (buffer pool frame
   ownership), 0010 (waiting for a frame instead of failing).
3. The relevant `crates/<crate>/README.md`, then the sibling `.MD` of
   each file being changed.

## Working from task.md

When asked to **"do next task"**, follow this procedure:

1. Read `.claude/task.md` — and nothing else as a work queue. It is the
   Coder's only inbox. If it contains a numbered subtasks list (1 to N),
   work through that list in the order given — that is the "recommended
   order." Start at the first subtask that is 🆕 New. Complete one
   subtask, then stop and hand control back for review before starting
   the next one. If `.claude/task.md` has no subtasks list, treat the
   whole file as a single task and do it in full.
2. Move the subtask's status as you work it, in `.claude/task.md` — and
   move only the status. Set 🚧 In Progress when you start it and
   👀 Review when you finish it, in both the Order Plan line and the
   `Status:` line under the subtask's heading. **Never set ✅ Done**: that
   is the Code Reviewer's, and a subtask marking itself done is the whole
   reason the review gate exists. Never reword, reorder, delete or "clean
   up" the task text itself — the Task writer owns that prose, exactly as
   it owns the file. A status marker is the one thing the Coder may write
   there.
3. `.claude/problems.md` is where incidental discoveries
   go: if, while working a subtask, you notice a problem that is real but
   does not depend on or belong to the subtask in progress, record it in
   `.claude/problems.md` rather than investigating or fixing it there. Fixing it
   is a separate, later task.
4. A subtask whose heading names a `P-<n>` is a scheduled problem. It is
   already gone from `.claude/problems.md` — the Task writer removed it
   when it wrote the subtask — so there is nothing to close there and
   nothing to look up: the subtask restates everything needed. Do not go
   hunting for the original entry.
5. Do not go beyond the subtask boundary implied by the order list — a
   subtask is done when its own scope is satisfied, not when adjacent
   related work is also finished.

When asked for the **"next task"** (the Task writer's job, not the
Coder's), the queue is: schedulable entries in `.claude/problems.md`
first, the next milestone in `docs/ROADMAP.md` second, and neither if the
current milestone is finished — in that last case the answer is to say so
and ask the human whether to hand the tree to the Milestone Reviewer, not
to invent work. "Schedulable" excludes anything signed `Created by:
Architect`: those are the Architect's own, and only the Architect turns
one into a task or resolves it. The full procedure is in
`.claude/agents/task-writer.md`.

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

1. **Coder** — asked to work `.claude/task.md`, which is the only file it
   takes work from. Do the tasks it lists, in the order its Order Plan
   gives, moving each subtask's status to 🚧 In Progress when it starts
   and 👀 Review when it stops — never to ✅ Done, which is the Code
   Reviewer's. Never commit automatically. Don't install heavy tooling (e.g. Python/pip)
   for investigating — use `bash` instead. If a problem surfaces that
   isn't part of the current subtask, record it in `.claude/problems.md` rather
   than investigating or fixing it there. If a needed investigation is
   itself large (e.g. testing a hypothesis), ask before doing it — that
   is usually Architect's work, not Coder's. `.claude/task.md` typically holds
   several subtasks tied to one milestone, or one subtask per open
   problem; work through them one at a time and stop after each one so it
   can be reviewed before the next starts.
2. **Architect** — asked to investigate an entry from `.claude/problems.md` or
   review the project as a whole (documentation, module structure,
   etc.). Records findings in that same file, as entries signed
   `Created by: Architect`, carrying their own evidence — file paths and
   line numbers — and what to do next. Those entries are the Architect's
   alone: nobody else schedules them and nobody else deletes them, and
   the Architect is the one role that may write `.claude/task.md` for its
   own entries. A conclusion that must outlive the working tree becomes
   an ADR, a roadmap entry or a paragraph here. Owns the project's
   cross-cutting prose and its process: `docs/adr/**`, the roadmap's
   entry text (never its status markers), this file, and
   `.claude/agents/*.md` plus `.claude/settings*.json`. Never touches a
   `.rs` file, a test, a sibling module `.MD` or a crate `README.md`.
3. **Task writer** — asked to turn a user request, the open entries in
   `.claude/problems.md`, or the next milestone into a task for the Coder
   role. Write `.claude/task.md` using the task format below: keep it
   understandable but short — the Coder role doesn't need root causes or
   other background, just the task. Decompose a large task or
   sub-milestone into several subtasks and order them with an Order
   Plan, each subtask starting at 🆕 New. Archives the finished
   `.claude/task.md` to `docs/tasks/` and sets 🚧 In Progress in
   `docs/ROADMAP.md` when it writes a milestone's first task.
4. **Code Reviewer** — asked simply to review, and works out the rest:
   find the subtask standing at 👀 Review in `.claude/task.md`, and
   review the **uncommitted** working tree, which is that subtask's work
   by construction — the human commits after each accepted subtask, so
   nothing else can be in there. Only the new code and documentation,
   never anything already committed and never the untouched code around
   it. If no subtask is at 👀 Review, or the tree is clean, there is
   nothing to review — say so rather than picking something. Then read
   the **code and its documentation** the way a
   human reviewer reads a pull request, asking four things in order: is
   it *finished* (every part of the subtask, not the easy parts), is it
   *correct* (this file's invariants first), is it the *right solution*
   (wrong layer, reinvented machinery, an abstraction that blocks the
   next milestone), and is it *well made* (conventions, Rust practice,
   naming, error handling, logging). All four apply to the `.MD` files as
   much as the `.rs`: documentation that no longer describes its code is
   a defect like any other. It does not review tests or check that the
   gate was run — both are the Coder's, and both are automated. Passing
   sets that
   subtask ✅ Done — it is the only role that may. Failing sets it back
   to 🚧 In Progress and records each finding in `.claude/problems.md`
   using the problem format below, signed `Created by: Code Reviewer`,
   with concrete instructions on how to fix it and how to prevent it
   recurring.
5. **Milestone Reviewer** — asked to review a finished milestone as a
   whole against its `docs/ROADMAP.md` entry, once every subtask has
   passed code review: the milestone's Done-when, cross-cutting
   invariants, documentation truth, forward dependencies it created, and
   deferred items. Writes `.claude/problems.md`, and is the only role
   that sets ✅ Done in `docs/ROADMAP.md`.
6. **Helper** — the default role: anything not covered by the five roles
   above, such as answering a question about the project. Read-only.

### Channels

Two files under `.claude/` carry the roles' communication, and together
with the roadmap each answers exactly one question. Keeping them to that
one question is what stops them turning into three overlapping logs:

- `.claude/problems.md` — what is wrong **right now**. Open problems
  only; an entry leaves the file when it is dealt with.
- `.claude/task.md` — the one task being worked at this moment.
- `docs/ROADMAP.md` — the whole project, milestone by milestone.

Each channel has one primary writer; the extra writers listed are
deliberate. Nothing is written that nobody reads — a channel with no
reader is a bug in this table, not a file to keep writing.

Their extension is lowercase `.md`, unlike the uppercase `.MD` of the
sibling module docs under `crates/`. The case is the fastest way to tell
which kind of file a path means, and `.gitignore` matches it literally on
a case-sensitive filesystem, so the difference is load-bearing rather
than cosmetic.

| File | Written by | Read by | Carries |
| --- | --- | --- | --- |
| `.claude/task.md` | Task writer (Architect for its own `Created by: Architect` entries, or when the human asks; Coder for 🚧/👀 status; Code Reviewer for ✅) | Coder, Code Reviewer, Milestone Reviewer | The current task's subtasks and their Order Plan |
| `.claude/problems.md` | Everyone who finds something: Coder, Code Reviewer, Milestone Reviewer, Architect | Task writer first, Architect, human | A queue of everything found and not fixed on the spot: defects from review, incidental discoveries, and the Architect's findings with their evidence |

Every finding goes to `.claude/problems.md`, whoever found it, signed
with `Created by:`. That line is the whole difference between a defect a
reviewer found, something the Coder noticed in passing, and an Architect
finding that needs a decision — one queue, one numbering scheme, one
place for the Task writer to look. An Architect entry carries its own
evidence and citations in the entry itself; there is no separate place
for that reasoning to accumulate.

Both files are in `.gitignore`. They are live working state, not history:
a channel entry is either scheduled into a task, or promoted into
something durable — an ADR under `docs/adr/`, a `docs/ROADMAP.md` entry,
this file — before it matters. The durable copies are `docs/tasks/**` for
finished tasks and `docs/adr/**` for decisions. A conclusion that has to
outlive the working tree is not finished until it is one of those, and
deciding what graduates is part of the investigation that produced it.

### Who owns which files

Every path in the repository has exactly one role that may change it.
"Owns" means: that role makes the change, and any other role that wants
it changed asks through a channel above.

| Area | Owner | Notes |
| --- | --- | --- |
| `crates/**/*.rs` — source and tests | Coder | The only role that writes Rust. Tests are not a separate area: a subtask's tests ship with its code. |
| `crates/**/*.MD` — sibling module docs | Coder | Ships in the same commit as its `.rs`. Whoever edits the code edits the doc. |
| `crates/*/README.md` | Coder | Same rule: it documents that crate's code, so it goes stale the moment code lands without it. |
| `README.md`, `CLAUDE.md` | Architect | Repository-level prose. "What works today" claims here are checked by the Milestone Reviewer at the end of each milestone. |
| `docs/adr/**` | Architect | A decision worth an ADR is recorded by the role that investigated it. |
| `docs/ROADMAP.md` — entry prose | Architect | Including retiring or splitting an entry. |
| `docs/ROADMAP.md` — status markers | Architect sets 🆕 on a new entry, Task writer sets 🚧, Milestone Reviewer sets ✅ | Nobody else. ✅ means the milestone's functionality was reviewed and works, which is the gate that makes "Done" mean something. |
| `docs/diagrams/**` | Architect | The map, not the contract: if a diagram disagrees with `CLAUDE.md` or `.claude/agents/`, the diagram is wrong. |
| `docs/tasks/*.md` — archived specs | Task writer | Written once when a task is archived, and history from then on. |
| `docs/tasks/README.md` | Architect | Not an archived spec: it explains what the archive is for, which is process prose and goes stale like any other. |
| `.claude/agents/*.md`, `.claude/settings*.json` | Architect | The roles' own definitions and Claude Code configuration. |
| `.claude/task.md` — prose | Task writer | The Architect writes it for its own `Created by: Architect` entries, or when the human explicitly asks, following `.claude/agents/task-writer.md` exactly either way. |
| `.claude/task.md` — subtask status | Task writer sets 🆕, Coder sets 🚧 and 👀, Code Reviewer sets ✅ | Each role moves the status only to its own rung, and only for the subtask it is working. See "Status, and who may set it". |
| `.claude/problems.md` | Whoever finds the problem | Every role may append a signed entry. Deletion is the only way an entry leaves — the file is an open queue, never a history — and who may delete follows the signature: the Task writer deletes what it schedules and never an Architect entry, the Architect deletes its own. |
| `.github/workflows/**`, `scripts/**`, `Cargo.toml`, `Dockerfile`, `.gitignore` | Coder | Executable configuration is code: it is changed through a task and reviewed as code. |

Milestone planning is the Task writer's, milestone review is the
Milestone Reviewer's, and neither is a file the other may write.

No role commits. Finished work is left in the working tree for the human
to review and commit.

### File formats

`.claude/task.md`:
- Title: milestone number + a short description, or `Problems` when the
  task is a batch of `P-` entries.
- Order Plan: a numbered list (1 to N) giving the subtask order, each
  line carrying its own status marker.
- A description for every subtask in the Order Plan, including how to
  test it, and a `Status:` line. A subtask that exists to fix a problem
  names its `P-<n>` in its heading.

### Status, and who may set it

Two ladders, one for subtasks and one for milestones. Each rung is owned
by exactly one role, and the point of that is the last rung: **nothing
marks its own work complete.**

A **subtask** in `.claude/task.md` carries one of four:

| Status | Meaning | Set by |
| --- | --- | --- |
| 🆕 New | written, not started | Task writer, when it writes the subtask |
| 🚧 In Progress | being worked right now | Coder, when it starts |
| 👀 Review | finished and awaiting review | Coder, when it stops |
| ✅ Done | reviewed and accepted | Code Reviewer, and nobody else |

A review that fails does not invent a fifth status: the Code Reviewer
puts the subtask back to 🚧 In Progress and records what is wrong in
`.claude/problems.md`, so the same subtask is picked up again rather than
the defect being scheduled as new work.

A **milestone or sub-milestone** in `docs/ROADMAP.md` carries one of
three:

| Status | Meaning | Set by |
| --- | --- | --- |
| 🆕 New | not started | Architect, when it writes the entry |
| 🚧 In Progress | started | Task writer, when it writes the milestone's first task |
| ✅ Done | reviewed and accepted | Milestone Reviewer, and nobody else |

On a sub-milestone, 🚧 means someone is writing code for it now, and at
most one sub-milestone anywhere carries it. On a parent, 🚧 means partly
delivered, so several parents may carry it at once; a parent becomes ✅
Done only when everything under it is.

✅ Done on a milestone means its functionality was reviewed and works —
not that its subtasks were all ticked. That is why only the Milestone
Reviewer sets it, and why the Task writer's "the milestone looks
finished" is a question to the human rather than a status change.

`.claude/problems.md` is a queue, not a log. The file holds exactly the
problems that have not yet been turned into work; an entry leaves it when
the Task writer schedules it, and the task — then the archived task under
`docs/tasks/` — becomes the record of what was found. Its head carries a
`Next entry:` line giving the next free `P-` number, because numbers are
never reused and the highest one in the file is no longer a reliable
guide once entries start leaving it.

Per entry:
- Title: `P-<n>` + a short description.
- `Created by:` the role that found it — Coder, Code Reviewer, Milestone
  Reviewer, or Architect. This line decides who acts on the entry, so it
  is not optional. **`Created by: Architect` means the entry belongs to
  the Architect**: something noticed that wants a judgement call, a design
  decision, or an investigation before anyone writes code. The Task writer
  neither schedules nor deletes one; the Architect resolves it, or writes
  the task for it itself, and reports it to the human either way. Every
  other signature is schedulable work.
- Reason: why it is a problem, in one or two sentences.
- Description: full detail, with file paths and line numbers. Assume the
  entry will be read once, by whoever writes the task, and then deleted —
  so it must contain everything needed to specify the fix. An Architect
  entry carries its evidence here too: what was checked, what was found,
  what the options are and which one is recommended. There is no second
  file for that reasoning to live in.
- How to prevent in future: a concrete instruction — a lint, a test, a
  CI step. Mandatory when the entry is a defect found in review;
  "be careful next time" is not a prevention. Omit it only for an entry
  that is an observation rather than a fault.

The file holds open problems only. There is no resolved state and no
`Status:` line: an entry that has been dealt with is deleted, not
annotated. Two roles delete, and the signature decides which. The Task
writer deletes an entry when the corresponding subtask exists in
`.claude/task.md`, and never touches an Architect one. The Architect
deletes its own: when the question is settled — and then whatever was
decided has already graduated to an ADR, a roadmap entry or this file,
because otherwise deleting the entry loses it — or when it has written
the task for it. Reading the file top to bottom should show exactly the
outstanding work and nothing else.

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
  binder, never enforced (M15).
- `common::SqlState::NOT_NULL_VIOLATION` — defined, never raised (M15).
- `common::SqlState::UNIQUE_VIOLATION` — defined, never raised (M16); the
  B+tree still permits duplicate keys.
- `storage::btree::BTreeIndex::delete` — the method exists; its body is
  `todo!()` (M14).
- `catalog::Catalog::drop_table` — the method exists
  (`crates/catalog/src/catalog.rs:125`); its body is `todo!()` and nothing
  in the tree calls it, since no grammar produces `DROP TABLE` (M26).
- `executor::NestedLoopJoinExecutor` — exists and is wired into the
  executor factory; `init` and `next` are both `todo!()` (M23.1).
  `planner::LogicalPlan::Join`/`PhysicalPlan::NestedLoopJoin` already
  exist as the node kinds it would run, but nothing in `sql`'s grammar
  can produce them yet — `FROM` accepts exactly one table (M23.1).
- `txn::LockManager` — implemented and unit-tested as of M10.2's first
  subtask, but nothing above it calls `lock`/`lock_table`/`release_all`
  yet; the executors and `TransactionManager` wire it up later in the
  same sub-milestone.
- `txn::VersionChain::visible_version` — `todo!()`; MVCC is M10.3.

Milestone numbers here are the roadmap's, and a roadmap number is a
priority in natural numeric order: an unstarted milestone is renumbered
when priorities change, while anything with shipped work under it keeps
its number. There is no M11 entry — an `M11` in a `.MD` file or a
`// TODO(M11):` marker is stale and means M23.1 (joins) or M23.2
(statistics, cost and composite keys). Retarget one when its file is
open.

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

The current exceptions are a known, closed set - five `#[allow(dead_code)]`
attributes across three files, none of them expressible in configuration
since each silences one specific field or item rather than a lint
crate-wide:

- `crates/executor/src/operators/nested_loop_join.rs`'s
  `NestedLoopJoinExecutor::{left, right, predicate}` mark work that
  belongs to a milestone not yet built (M23.1's join executor,
  `docs/ROADMAP.md`) and should disappear when that milestone lands and
  starts reading them.
- `crates/storage/src/disk.rs`'s `DiskManager::path` and
  `crates/storage/src/replacer.rs`'s `LruKReplacer::capacity` are fields
  kept for future use that nothing reads yet.

Do not add a sixth without updating this list. `LockManager::holders` was
the sixth until M10.2's first subtask made it live; a suppression that
becomes unnecessary is deleted here as well as in the source.

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
