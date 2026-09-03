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
  and the four channel files, with who writes and who reads each. Read
  this one first.
- [`agent-flow-milestone-planning.mmd`](agent-flow-milestone-planning.mmd)
  — roadmap entry to `.claude/task.MD`, via an investigation when the
  milestone's shape is not settled.
- [`agent-flow-task-implementation.mmd`](agent-flow-task-implementation.mmd)
  — the Coder's loop: one subtask, the gate, stop for review. Includes
  the three conditions that stop the Coder instead.
- [`agent-flow-code-review.mmd`](agent-flow-code-review.mmd) — diff-level
  review of one subtask, in check order.
- [`agent-flow-bug-fixing.mmd`](agent-flow-bug-fixing.mmd) —
  `.claude/bugs.MD` to a fix that ships with its prevention.
- [`agent-flow-milestone-review.mmd`](agent-flow-milestone-review.mmd) —
  whole-milestone review and the only path to ✅ Done.
- [`agent-flow-documentation.mmd`](agent-flow-documentation.mmd) — who
  owns which documentation, and the four layers that validate it.
- [`agent-flow-investigation.mmd`](agent-flow-investigation.mmd) — the
  Architect's loop: evidence, options, recommendation, and where the
  recommendation goes next.
- [`agent-flow-questions.mmd`](agent-flow-questions.mmd) — routing an
  incoming request to the role that owns it.

A flow diagram that disagrees with `.claude/agents/` or `CLAUDE.md` is
wrong by definition: those two are the contract, these are the map.
