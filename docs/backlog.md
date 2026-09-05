# Backlog

Problems this project knows about and is not going to do. Every entry
here was an open entry in `.claude/problems.md`, was triaged — importance,
effort, decision — and came out **Backlog**: real, understood, and not
worth what fixing it costs right now.

This file is checked in; `.claude/problems.md` is gitignored working
state that holds only what is still to be done. So an entry that simply
stayed in the queue would be lost the moment someone emptied it, and one
that was deleted would be re-found and re-filed a few milestones later.
This is where it goes instead.

## How an entry gets here

Only the Architect writes this file, and only through the triage the
human asks for — normally when a task, a sub-milestone or a milestone is
finished. The full protocol is `CLAUDE.md`'s "Problem triage"; in short:

1. The Architect annotates **every** entry in `.claude/problems.md` with
   a `Triage:` line — importance, effort in story points, decision — and
   stops. Nothing is moved or deleted in that pass.
2. The human reviews the estimates, edits any of them by hand, and
   approves.
3. The Architect then moves every entry marked `decision Backlog` here,
   deleting it from `.claude/problems.md` in the same edit. What is left
   in the queue is the will-do list.

The human's edit wins at step 3. If the Architect disagrees with a
changed decision it says so in its reply and moves the entries as the
file stands.

## The three criteria

**Importance** — how much the problem matters:

- `High` — correctness, durability, data loss, a broken invariant from
  `CLAUDE.md`, a rule about logging user data, or something a later
  milestone will otherwise build on top of. Never backlogged, whatever
  it costs.
- `Medium` — a real defect or a false statement in documentation, but
  with a workaround, a narrow blast radius, or no user reaching it yet.
- `Low` — cosmetic, tidying, a limitation nobody has hit.

**Effort** — classic story points, Fibonacci, judged as work for the
Coder including its tests and `.MD` updates:

- `1` — a line or two in one file. A doc sentence, a stale reference.
- `2` — one file plus a test, no design thinking.
- `3` — a few files inside one crate, tests, no cross-crate effects.
- `5` — several files, a crate boundary crossed, or a new test harness.
- `8` — cross-cutting, or touching storage/WAL/recovery/the buffer pool,
  which means the crash-injection sweeps must run.
- `13` — needs a design decision or an ADR before any code. Usually the
  sign that it is a roadmap milestone rather than a problem entry; say
  so instead of backlogging it.

**Decision** — `Will do` or `Backlog`, and it follows from the pair, not
from either half:

- `High` importance → `Will do`, at any effort. Cost decides when, not
  whether.
- Effort `1`–`2` → `Will do` even at `Low` importance: scheduling it
  costs about what arguing about it costs.
- `Low` importance and effort `5`+ → `Backlog`. This is the case the
  file exists for.
- `Medium` importance and effort `5`+ → a judgement call. Say which way
  and why in one clause, because this is the row the human is most
  likely to overturn.

An entry that names only one of the two criteria has not been triaged.

## What does not belong here

- Unfinished work with a milestone that closes it — that is
  `docs/ROADMAP.md`. Code that exists but cannot be reached yet is
  `CLAUDE.md`'s "Known scaffolding".
- Anything still expected to be fixed. If someone will schedule it, it
  stays in `.claude/problems.md`.
- A decision whose reasoning a later change must not violate. That is an
  ADR under `docs/adr/`; the entry here links to it rather than carrying
  the argument.

## Reading it

Read this file before filing a finding: something already here has been
triaged once and re-filing it re-runs a decision that was made. The
Milestone Reviewer checks it before opening entries and does not fail a
milestone on a backlogged problem. The Task writer never schedules from
it — it is a record, not a queue.

An entry leaves only when the Architect takes it out: because the problem
stopped being true, or because something changed its importance — a
milestone that now depends on it, a user who now hits it — and then the
same edit files a fresh `P-` entry in `.claude/problems.md`, since
nothing else would pick it up again.

## Entries

Each one is `## <short title>`, one or two sentences saying what is wrong
and why it is not being done, and the triage it came out of on its own
line. No `P-` number: those belong to the queue, are never reused, and
would only send a reader looking for an entry that is no longer there.

```
## Sequential scan re-reads the page header per tuple

Every `next()` re-decodes the slot directory instead of caching it, so a
full scan does measurably redundant work; the fix touches the heap
iterator and its crash tests for a cost nothing has yet noticed.

Importance: Low · Effort: 8 SP
```

_None yet._
