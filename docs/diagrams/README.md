# Diagrams

Mermaid sources, one flow per `.mmd` file. Nothing renders them in this
repository and CI does not check them — GitHub and most IDEs render
`.mmd` directly, and that is the intended way to read them.

## Structure

- [`crate-dependencies.mmd`](crate-dependencies.mmd) — the layered
  workspace and every allowed dependency edge, including `test-support`'s
  dev-dependency-only edges drawn dashed. The authoritative list is the
  table in `CLAUDE.md`; this is its picture. See
  [`../adr/0002-crate-splitting.md`](../adr/0002-crate-splitting.md) for
  why the layers are crates rather than modules.

## Agent flows

How the six LLM roles in `CLAUDE.md`'s "LLM roles and channels" section
actually move work between each other. Each role's own rules live in
`.claude/agents/<role>.md`; these diagrams are the view across roles that
no single agent file has.

- [`agent-flow-overview.mmd`](agent-flow-overview.mmd) — all six roles
  and the two channel files, with who writes and who reads each. Read
  this one first.
- [`agent-flow-milestone-planning.mmd`](agent-flow-milestone-planning.mmd)
  — how the Task writer decides what the next task is: open problems
  first, then the next roadmap entry, and a question to the human rather
  than a task when the milestone is already finished.
- [`agent-flow-task-implementation.mmd`](agent-flow-task-implementation.mmd)
  — the Coder's loop: one subtask, the gate, mark it done, stop for
  review. Includes the three conditions that stop the Coder instead.
- [`agent-flow-code-review.mmd`](agent-flow-code-review.mmd) — diff-level
  review of one subtask, in check order.
- [`agent-flow-problem-lifecycle.mmd`](agent-flow-problem-lifecycle.mmd)
  — a finding by any role, into `.claude/problems.md`, out of it again as
  a scheduled subtask, to a fix that ships with its prevention. Also
  shows the other exit: an Architect-signed entry, which the Task writer
  never touches and the Architect either settles or writes the task for
  itself.
- [`agent-flow-milestone-review.mmd`](agent-flow-milestone-review.mmd) —
  whole-milestone review and the only path to ✅ Done.
- [`agent-flow-documentation.mmd`](agent-flow-documentation.mmd) — who
  owns which documentation, and the four layers that validate it.
- [`agent-flow-investigation.mmd`](agent-flow-investigation.mmd) — the
  Architect's loop: evidence, options, recommendation, and where the
  recommendation goes next — fixed on the spot, written into
  `.claude/problems.md` as a signed entry, or graduated to an ADR.
- [`agent-flow-questions.mmd`](agent-flow-questions.mmd) — routing an
  incoming request to the role that owns it.

A flow diagram that disagrees with `.claude/agents/` or `CLAUDE.md` is
wrong by definition: those two are the contract, these are the map.
