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
   `Importance:` and `Effort:` there and then, using the scales below.
   Those two are the only fields a filer writes.
2. The Architect goes through **every** entry, corrects an `Importance:`
   or `Effort:` the filer got wrong, and adds the two fields only it may
   write — `Thinking:` and `Decision:` — then stops. Nothing is moved or
   deleted in that pass. An entry with no `Thinking:` line has not been
   through this pass, and nobody schedules it.
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

**Thinking** — how much reasoning the fix needs, on a scale of **1 to
10**, written as a bare number: `Thinking: 7`. It decides *who* gets the
entry, and unlike importance and effort it is **written only by the
Architect**, in a triage or when filing one of its own entries, and
changed by the human. No other role writes it, and an entry without it is
not scheduled by anyone.

- `1` — one obvious edit. A typo, a stale sentence, a renamed symbol.
- `2` — a localized change with the fix already stated in the entry.
- `3` — several files, following a pattern that already exists in the
  tree to copy from.
- `4` — the design is given, but carrying it out means holding a
  subsystem's invariants in mind: the WAL ordering rule, latch ordering,
  a crash-injection sweep that has to keep passing.
- `5` — the fix is known but its consequences cross crates, so where it
  belongs is a judgement call.
- `6` — a choice between options the entry has already enumerated.
- `7` — a design question whose options are not enumerated yet; finding
  them is part of the work.
- `8` — a boundary has to move: a module decomposition, an ownership
  change between crates, a data structure whose key or lifetime changes.
- `9` — a decision that constrains later milestones and needs an ADR
  before any code is written.
- `10` — a change to the project's own model: an invariant in
  `CLAUDE.md`, the durability contract, the role process itself.

**`1`–`4` is Coder work** and reaches the Coder through the Task writer.
**`5`–`10` is Architect work** and reaches the Architect **through the
human**, never automatically. The line between 4 and 5 is one question:
*is the answer known before the work starts?* If yes it is at most a 4,
however long the work is; if no it is at least a 5, however short.

The level is about the thinking, not the size: a 1 SP entry is a `9` when
the single line to change is obvious only once the decision is made, and
an 8 SP entry is a `3` when it is long but entirely settled. An entry
that is a decision *followed by* mechanical work takes the number of the
decision, because that half comes first and produces its own task.

**Decision** — `Will do` or `Backlog`, written only by the Architect in a
triage or by the human, never by the role that filed the entry. It
follows from importance and effort, not from either half, and not from
the thinking level — a `9` is done or backlogged on the same grounds as a
`2`:

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
missing estimate, is what the next triage looks for, since `Importance:`
and `Effort:` are there from the moment the entry is filed. `Thinking:`
is missing on exactly the same entries, for the same reason: both are the
Architect's to write.

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

Thinking: 3

---
```

_None yet._
