# AGENTS.md

Shared entry point for Codex and Claude Code. Claude imports this file.
Detailed policy lives in `docs/agent-guide.md`; role procedures live in
`docs/agents/`. Read the sections relevant to the requested work before
acting. These are instructions, not optional background.

## Route the request

Start every project reply with `Role: <role name>`. Select the role from
the request; a main session can change roles between requests. A deliberately
selected subagent keeps its assigned role and reports a routing mismatch.

| Request | Role | Read before acting |
| --- | --- | --- |
| Work the current task / "do next task" | Coder, or Architect if its For line says so | `.claude/task.md`, then `docs/agents/coder.md` or `docs/agents/architect.md` |
| Investigate, review project structure, edit agent setup, triage problems | Architect | `docs/agents/architect.md` |
| Write the next task | Task writer | `docs/agents/task-writer.md` |
| Review a finished milestone | Milestone Reviewer | `docs/agents/milestone-reviewer.md` |
| Questions and explanations | Helper (read-only) | `docs/agents/helper.md` |

Roles are procedures, not an instruction to spawn agents. Delegate only
when requested. Never run concurrent writers against the shared queue.

## Shared working state and review gates

- Both tools use **`.claude/task.md` and `.claude/problems.md`**. The
  directory name is historical. Do not create a second queue in `.codex/`.
  Existing content belongs to the human; never overwrite it during setup.
- Missing task means no implementation is scheduled. The Task writer may
  create it when requested. Missing problems means an empty queue; the
  first finding creates it with `Next entry: P-2` and entry `P-1`.
  Existing queues allocate from their `Next entry:` counter.
- Read the task's `For:` first. Resume 🚧 before taking 🆕; never skip an
  earlier 👀 Review. Work **one subtask**, update only its status in both
  places, finish at 👀 Review, and stop. Only the human sets task ✅ Done.
- The Task writer writes only into an empty task file and schedules only
  triaged `Thinking:` + `Decision: Will do` problems: 1–7 for Coder,
  then 8–10 for Architect. Never mix roles in one task.
- Triage rates entries first; moving Backlog entries requires the human's
  approval. Read `docs/backlog.md` before filing a finding; never duplicate
  or revive its entries without approval.
- Only the Milestone Reviewer sets a parent milestone ✅ Done.
- Never commit automatically. Preserve unrelated edits.

## Before changing the engine

Read `docs/ROADMAP.md`, relevant ADRs, the crate README and each changed
module's sibling `.MD`. Read the guide's engineering sections from
"Invariants that must not be broken" through "Testing rules".
The current capability summary is the guide's "What this is";
`CLAUDE.md` holds the checker-readable "Known scaffolding" list.

Critical rules: log before page; preserve zero-page validity and header
versioning; obey buffer latch ordering; never reacquire a page write guard
on the same thread. Keep statement/catalog locks out of data-page flushes,
with the documented double-write serialization exception. Read ADR 0004
before making isolation claims. Log errors once at the engine boundary,
never log row data, and never log raw SQL above DEBUG.

Every Rust source and test has an uppercase sibling `.MD`, updated together.
Source comments are limited to SAFETY and milestone TODO markers. Follow
the guide's crate-edge policy, lint rules, documentation format and test
placement rules.

## Validation and tools

Code/build/script/CI changes run the full gate at the end:

```sh
cargo build --workspace
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
bash scripts/check_docs.sh
cargo test --workspace --no-fail-fast
```

Prose changes run the documentation checker; agent configuration changes
also validate their JSON/TOML and referenced paths. No Rust suite is needed
for agent configuration alone. Tests have a three-execution budget per
subtask, including targeted runs; a third failure leaves 🚧 and becomes
a problem entry. Storage/WAL/recovery/buffer changes run both crash sweeps.
Tests use deterministic synchronization, never repeated shell test loops.

Use available native file tools, or `rg` and scoped shell reads. Prefer
`apply_patch` in Codex and Edit/Write in Claude. Use Bash for the repository
scripts (Git Bash or WSL on Windows). Do not copy Claude's permission rules
into Codex; each host manages permissions separately.

Setup details and audit results: `docs/agent-setup.md`.
