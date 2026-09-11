# Diagrams

Mermaid sources, one flow per `.mmd` file. Nothing renders them in this
repository and CI does not check them — GitHub and most IDEs render
`.mmd` directly, and that is the intended way to read them.

## Structure

- [`crate-dependencies.mmd`](crate-dependencies.mmd) — the layered
  workspace and every allowed dependency edge, including `test-support`'s
  dev-dependency-only edges drawn dashed. The authoritative list is the
  table in `docs/agent-guide.md`; this is its picture. See
  [`../adr/0002-crate-splitting.md`](../adr/0002-crate-splitting.md) for
  why the layers are crates rather than modules.

## Agent flows

How the five LLM roles in `docs/agent-guide.md`'s "LLM roles and channels" section
actually move work between each other. Each role's own rules live in
`docs/agents/<role>.md`; these diagrams are the view across roles that
no single agent file has.

- [`agent-flow-overview.mmd`](agent-flow-overview.mmd) — all five roles
  and the two channel files, with who writes and who reads each, plus the
  human, who reviews each finished subtask themselves. Read this one
  first.
- [`agent-flow-milestone-planning.mmd`](agent-flow-milestone-planning.mmd)
  — how the Task writer decides what the next task is: open problems
  first; an empty problems file closes the current sub-milestone and
  starts the next one; and a question to the human rather than a task
  when the parent has no sub-milestone left.
- [`agent-flow-task-implementation.mmd`](agent-flow-task-implementation.mmd)
  — the Coder's loop: one subtask, the gate, mark it 👀 Review, stop for
  review. Includes the three conditions that stop the Coder instead.
- [`agent-flow-problem-lifecycle.mmd`](agent-flow-problem-lifecycle.mmd)
  — a finding by any role, into `.claude/problems.md`, out of it again as
  a scheduled subtask, to a fix that ships with its prevention. Also
  shows the other two exits: a settled Architect entry whose conclusion becomes durable,
  and a triaged entry scheduled for its rated role, and the triage — the Architect estimating every entry,
  the human approving, and the backlogged ones moving to
  `docs/backlog.md` — plus the one way back out of that file, which is a
  revive the human has approved.
- [`agent-flow-milestone-review.mmd`](agent-flow-milestone-review.mmd) —
  whole-milestone review, the only path to ✅ Done on a parent, and the
  bugs and gaps it files when a milestone does not pass. Shows the two
  things that make the loop terminate
  (`docs/adr/0015-milestone-review-terminates.md`): the split between a
  gating finding, which holds the parent at 🚧, and a non-gating one,
  which the milestone passes with; and the second pass, scoped to the
  repairs rather than re-auditing the tree.
- [`agent-flow-documentation.mmd`](agent-flow-documentation.mmd) — who
  owns which documentation, and the four layers that validate it.
- [`agent-flow-investigation.mmd`](agent-flow-investigation.mmd) — the
  Architect's loop: evidence, options, recommendation, and where the
  recommendation goes next — fixed on the spot, written into
  `.claude/problems.md` as a signed entry, or graduated to an ADR.
- [`agent-flow-questions.mmd`](agent-flow-questions.mmd) — routing an
  incoming request to the role that owns it.

A flow diagram that disagrees with `docs/agents/` or `docs/agent-guide.md` is
wrong by definition: those two are the contract, these are the map.
