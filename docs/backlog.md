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

1. Whoever files an entry in `.claude/problems.md` fills its
   `Importance:`, `Effort:` and `Thinking:` there and then, using the
   scales below. Nobody but the Architect and the human writes its
   `Decision:`.
2. The Architect goes through **every** entry, corrects an `Importance:`,
   `Effort:` or `Thinking:` the filer got wrong, adds the `Decision:`
   line, and stops. Nothing is moved or deleted in that pass.
3. The human reviews the estimates, edits any of them by hand, and
   approves.
4. The Architect then moves every entry whose `Decision:` reads `Backlog`
   here, deleting it from `.claude/problems.md` in the same edit. What is
   left in the queue is the will-do list.

The human's edit wins at step 4. If the Architect disagrees with a
changed decision it says so in its reply and moves the entries as the
file stands.

## The four criteria

**Importance** — how much the problem matters. Each level carries an
icon, and the icon is written with the word, never instead of it, so the
line stays greppable: `Importance: 🔴 High`.

- `🔴 High` — correctness, durability, data loss, a broken invariant from
  `CLAUDE.md`, a rule about logging user data, or something a later
  milestone will otherwise build on top of. Never backlogged, whatever
  it costs.
- `🟡 Medium` — a real defect or a false statement in documentation, but
  with a workaround, a narrow blast radius, or no user reaching it yet.
- `🟢 Low` — cosmetic, tidying, a limitation nobody has hit.

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

**Thinking** — how much reasoning the fix needs, which is what decides
*who* gets it. Written by whoever files the entry, corrected by the
Architect in a triage, and changed by the human at any time:

- `🔧 Low` — the answer is already known and the work is carrying it out:
  writing code against a stated design, updating documentation, a small
  localized fix, a renamed symbol, a stale sentence. This is Coder work,
  and the Coder runs a smaller model at lower effort
  (`.claude/agents/coder.md`'s front matter) precisely because these
  entries do not need more.
- `🧠 High` — the answer is not known yet and finding it is most of the
  job: a module decomposition, a boundary that has to move, a choice
  between designs with consequences, anything whose entry reads as
  options rather than an instruction. This is Architect work at full
  reasoning effort, and it reaches the Architect **through the human**,
  never automatically.

The two levels are about the *thinking*, not the size: a 1 SP entry can
be `🧠 High` when the one line to change is obvious only after the
decision is made, and an 8 SP entry can be `🔧 Low` when it is a long but
settled piece of work. When an entry is genuinely both — a decision
followed by mechanical work — file it `🧠 High`, because the decision
comes first and produces its own task.

**Decision** — `Will do` or `Backlog`, written only by the Architect in a
triage or by the human, never by the role that filed the entry. It
follows from importance and effort, not from either half, and not from
the thinking level — a `🧠 High` entry is done or backlogged on the same
grounds as any other:

- `High` importance → `Will do`, at any effort. Cost decides when, not
  whether.
- Effort `1`–`2` → `Will do` even at `Low` importance: scheduling it
  costs about what arguing about it costs.
- `Low` importance and effort `5`+ → `Backlog`. This is the case the
  file exists for.
- `Medium` importance and effort `5`+ → a judgement call. Say which way
  and why in one clause, because this is the row the human is most
  likely to overturn.

An entry with no `Decision:` line has not been triaged — that, not a
missing estimate, is what the next triage looks for, since `Importance:`,
`Effort:` and `Thinking:` are there from the moment the entry is filed.

Each field goes on its own line with a blank line between, in the queue
and here alike, never joined into one. An entry in `.claude/problems.md`
carries all four, in the order `Importance:`, `Effort:`, `Thinking:`,
`Decision:`; an entry here carries `Created:`, `Importance:`, `Effort:`
and `Thinking:`, its decision being the file it is in. A `Decision:` may
carry a clause of reason and may wrap onto the next line; the others are
a single word, a single number, or an icon and a word.

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
triaged once and re-filing it re-runs a decision that was made. **A
duplicate of a backlog entry is never opened in `.claude/problems.md`**,
by any role — it is a revive with the approval step skipped, and it
works, because the new entry gets triaged as if the earlier decision had
never happened. A role that believes a backlogged problem now matters
says so in its reply and files nothing. The Milestone Reviewer checks
this file before opening entries and does not fail a milestone on a
backlogged problem. The Task writer never schedules from it — it is a
record, not a queue.

An entry leaves this file in one of exactly two ways, and neither is an
agent's own judgement:

- **The human approves a revive.** Something changed its importance — a
  milestone that now depends on it, a user who now hits it. The Architect
  may propose this and must wait for the answer; no agent moves an entry
  back into `.claude/problems.md` unasked. Once approved, the Architect
  deletes it here and files a fresh `P-` entry citing this one, in the
  same edit, since nothing else would pick it up again.
- **The problem stopped being true.** The code it describes is gone, so
  there is nothing to revive and nothing re-enters the queue. The
  Architect deletes it and reports that it did.

## Entries

Each one is `## <short title>`, one or two sentences saying what is wrong
and why it is not being done, then `Created:` — the date the triage put
it here, `YYYY-MM-DD`, so a reader can tell a decision made last week
from one made ten milestones ago — and the three criteria it came out
with, one field per line, as in the queue. Entries are separated from each
other by a `---` rule, so nothing about where one ends is left to
guesswork. No `P-` number: those belong to the queue, are never reused,
and would only send a reader looking for an entry that is no longer
there.

```
## Sequential scan re-reads the page header per tuple

Every `next()` re-decodes the slot directory instead of caching it, so a
full scan does measurably redundant work; the fix touches the heap
iterator and its crash tests for a cost nothing has yet noticed.

Created: 2026-09-06

Importance: 🟢 Low

Effort: 8 SP

Thinking: 🔧 Low

---
```

_None yet._
