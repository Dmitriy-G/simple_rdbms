# Backlog

Problems this project knows about and is not going to do: each was an
entry in `.claude/problems.md`, was triaged, and came out `Backlog` —
real, understood, and not worth what fixing it costs right now. Read it
before filing a finding, because re-filing one of these re-runs a
decision that was already made. An entry leaves only when the human
approves a revive or when the problem stops being true. The process
around it — who writes here, the four criteria every entry carries, how a
revive works — is in `CLAUDE.md`'s "Problem triage".

---

## Each role should work to an explicit, staged workflow

A role definition today says what a role may write and what it must
produce, but not the order it should work in, so an agent improvises its
own sequence — investigate, edit, test, re-test — and the transcript
shows it. Naming stages per role (investigation, then change, then gate)
would make that visible and reviewable. Not being done because it is a
rewrite of all five role definitions to buy tidier transcripts rather
than a different outcome, and because the ordering that actually matters
is already enforced where it counts: one subtask at a time, status moved
before and after, the full gate at the end.

Created: 2026-09-07

Importance: 🟢 Low

Effort: 2 SP

Thinking: 8

---
