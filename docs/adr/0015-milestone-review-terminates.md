# ADR 0015: A milestone review terminates — gating findings, and a bounded second pass

Date: 2026-09-08

Status: Accepted

## Context

**This section describes the review loop as it stood at commit `144700f`,
before this decision.** No parent milestone had ever reached ✅ Done.

The Milestone Reviewer's failure path was: file every bug and every gap in
`.claude/problems.md`, leave the parent at 🚧 In Progress, and let the
milestone "come back to you once it is empty again". Three mechanisms
combined to make that loop non-terminating, and only the first is obvious.

**The re-review has no memory, so it re-samples.** A pass runs as a fresh
session with no record of what the previous pass covered. Nothing durable
survives it: the entries it filed are deleted by the Task writer when they
become subtasks, and its reply is gone by the next session. Its checks were
whole-tree rather than diff-scoped — "every ADR referenced anywhere
exists", "re-reading every entry that names it", "grep `docs/adr/` and
`docs/ROADMAP.md` for the model the milestone replaced". A whole-tree audit
of a growing tree under a context budget is necessarily sampled, so pass 2
over an unchanged tree covers different ground than pass 1 and finds things
that were equally true during pass 1. That is the design's expected output,
not a reviewer being lazy the first time.

**The repairs create genuine new findings.** A repair for a stale citation
is new tree state that has never been reviewed, and two of the four stale
citations one repair entry collected "were correct when written and broken
by a commit that landed after them". So even a hypothetically exhaustive
pass 1 cannot pre-empt pass 2.

**Nothing distinguished a milestone-gating finding from an incidental
one.** Every finding, down to a 1 SP stale line number in an ADR, held the
parent at 🚧. Combined with the two mechanisms above, the loop terminated
only if a pass found zero true statements to file about a tree that had
just changed.

The live instance at the time: M10 stood at 🚧 with M10.1 through M10.4 all
✅ Done and its Solution and Done-when met, held open by a single entry
listing four stale line numbers in three ADRs — a class of defect a later
commit re-breaks by construction. Meanwhile `docs/agent-guide.md` requires that
exactly one parent carry 🚧, so an unclosable milestone blocks every
milestone after it.

The forcing question is therefore: **what does ✅ on a parent assert, and
what may hold it open?** An exit condition stated as "this pass found
nothing" is not one a reviewer can drive towards, because it is a claim
about an unbounded search rather than about a finite set of checks.

Alternatives considered and rejected:

- **Leave it.** ✅ on a parent stops meaning anything and the roadmap loses
  its answer to "what is finished".
- **Persist a review report** under `docs/reviews/`. A new checked-in
  channel, against `docs/agent-guide.md`'s rule that nothing is written that nobody
  reads, when the only fact a later pass needs is one line.
- **Enumerate the checks as a finite checklist and stop there.** Worth
  doing, and adopted below as part of the decision, but alone it does not
  stop the repair diff from producing new findings on pass 3, 4, 5.

## Decision

**A milestone review is a bounded procedure with a stated exit condition:
a full pass, then a repair, then a verification pass scoped to that
repair.** Two rules make it bounded, and neither works without the other.

**1. Only a gating finding holds a parent at 🚧.** A finding is gating when
it violates a named line of the milestone's own Done-when, its Solution, or
an invariant in `docs/agent-guide.md`'s "Invariants that must not be broken". Every
other finding — a stale citation, a documentation sentence that is merely
imprecise, a limitation nobody has hit, anything belonging to a different
milestone — is filed in `.claude/problems.md` exactly as before, triaged
and scheduled like any other problem, and **the milestone passes with it
open**. The reviewer states which of the two each entry it files is, and
names the Done-when line or invariant for every gating one; an entry whose
gating claim cannot name that line is not gating.

This is what makes termination possible at all, because it replaces "the
queue is empty" — a condition no growing tree ever reaches — with a
condition about a finite, named list.

**2. Every pass after the first is scoped to the repairs.** Pass 1 is the
full-milestone audit. Pass 2 checks exactly two things: that each gating
entry's repair landed and does what the entry asked, and what those repair
commits changed. A finding outside that scope is filed and is never gating,
whatever it is, and a finding inside it may be gating and starts another
repair-and-verify round. The loop is therefore finite by construction: each
round can only be opened by a defect in the previous round's own repair
diff, which is a shrinking target rather than an unbounded re-audit.

**3. The pass records what it did on the milestone's roadmap entry.** Pass
2 needs to know that pass 1 happened and what it gated on, and the reply
that would have said so is gone. The anchor is one line appended by the
Milestone Reviewer to the milestone's own `docs/ROADMAP.md` entry when a
pass does not pass:

```
Reviewed: 2026-09-07 — full pass; gating: P-48; non-gating: P-50, P-51
```

A later failing pass appends its own line below it. Every `Reviewed:` line
on the entry is deleted in the same edit that sets the parent to ✅ Done, so
the roadmap keeps no review history — the line exists to carry one fact
across one gap between sessions, and a finished milestone has no gap left.
This is a new write permission for the Milestone Reviewer inside an
Architect-owned file, granted the same way its parent-✅ permission already
is, and it is recorded in `docs/agent-guide.md`'s ownership table rather than left
implicit.

**4. The checks are a finite enumerated list, reported in full.** The
reviewer's six checks are a closed list, and each pass reports every one of
them in its reply with what it covered and what it found — including "no
findings". A pass's coverage is then visible to the human at the moment the
verdict is given, which is the only point at which anybody can tell a check
that found nothing from a check that was never run.

## Consequences

✅ on a parent milestone becomes reachable, and it asserts something
narrower and more honest than before: **the milestone's stated
functionality was reviewed as a whole and works, and nothing gating is open
against it.** It does not assert that the tree is free of known problems —
`.claude/problems.md` and `docs/backlog.md` say that, and they never say it
is empty.

The reviewer now makes a judgement it did not make before: gating or not.
That judgement is checkable rather than free, because a gating entry must
name the Done-when line or invariant it breaks, and an entry that cannot is
not gating. The failure mode this opens — a real defect classified
non-gating and a milestone closed over it — is bounded by the same naming
rule and by the human, who reads the verdict and the enumerated checklist
before the ✅ is committed.

Non-gating findings do not disappear: they enter the ordinary queue, where
`Importance:` decides them. A 🔴 High finding is still done at any cost,
whether or not it gated a milestone, because the queue's own rules say so
and `Decision:` is never argued from one criterion alone.

`docs/agents/milestone-reviewer.md` carries the procedure,
`docs/agent-guide.md`'s "Status, and who may set it" and its ownership table carry
what ✅ asserts and who may write `Reviewed:`, and
`docs/diagrams/agent-flow-milestone-review.mmd` draws it. This ADR is what
those three are checked against when they disagree.

The general rule underneath, which any review loop this project adds later
should be read against before it is written down: **an exit condition is
stated as a finite set of checks against a named scope, never as the
absence of findings.** The absence of findings is not observable by a
sampled reader of a growing tree.
