# Claude Code and Codex setup

Both tools use the same five roles and human review process. Start sessions
from the repository root. `AGENTS.md` routes the request; `CLAUDE.md` imports
it. Read the requested role's procedure under `docs/agents/` and the relevant
sections of `docs/agent-guide.md`. The guide preserves the detailed engineering
rules and workflow previously embedded in the root instruction file; it is
loaded as needed, not imported in full into every conversation.

## Files and ownership

| Path | Purpose |
| --- | --- |
| `AGENTS.md` | Shared startup instructions and routing |
| `CLAUDE.md` | Claude import plus the canonical scaffolding list read by the documentation checker |
| `docs/agent-guide.md` | Detailed shared project policy |
| `docs/agents/*.md` | One authoritative procedure per role |
| `.claude/agents/*.md` | Claude frontmatter and pointers to shared procedures |
| `.claude/settings.json` | Claude project permissions |
| `.codex/agents/*.toml` | Codex custom agents and pointers to shared procedures |
| `.codex/config.toml` | Enables Codex agents; inherits model and permissions |
| `.claude/task.md`, `.claude/problems.md` | Shared, gitignored working state for both tools |

The Architect maintains instructions, role definitions and host configuration.
Changes to shared procedures apply to both hosts. Keep host adapters small;
do not paste the procedure into each adapter again.

The `.claude` queue paths are retained to preserve existing work and avoid
divergent queues. Use lowercase `.codex` for Codex configuration on every OS.
There is no `.codex/task.md`, `.codex/problems.md` or Codex `settings.json`.
The legacy `.claude/investigations.md` is not an active channel; existing
contents remain untouched. Do not schedule from it.

Ignored working state does not travel to a fresh clone or a new worktree.
Use the existing checkout for ongoing task work, or explicitly transfer the
current queue before handing work to another checkout. Never let two sessions
write the task or allocate problem numbers concurrently. Missing files are
handled as described in `AGENTS.md`; never fabricate a task to fill a gap.

## Using the roles

In either tool, ordinary prompts select the main session's role:

- "Do next task" reads the task's `For:` line and works one subtask.
- "Write the next task" selects Task writer.
- "Triage .claude/problems.md" selects Architect and stops after rating
  entries for human review; backlog moves follow approval.
- "Review milestone M…" selects Milestone Reviewer.
- A project question selects the read-only Helper.

Claude also supports explicit selection such as `claude --agent architect`.
Its adapters use the `opus` and `sonnet` aliases rather than pinning account-
dependent model IDs. There is no project-wide forced Helper agent: that
read-only role must not trap requests that require another role's edits.

Codex automatically discovers project custom agents in `.codex/agents/`.
All five are provided, including `task-writer`. These define delegated roles;
they do not force the main session into that role. Ask explicitly for a
subagent when delegation is wanted. Otherwise the main session follows the
same procedures directly. New sessions may be needed to load changed agents;
project configuration also depends on the host trusting the checkout.

Claude permissions and Codex sandbox/approval settings are independent.
The Codex project file does not lower approval requirements, grant network
access, or copy Claude's Bash allowlist. Helper has a read-only Codex sandbox;
the other roles inherit host permissions and enforce their write targets
through their instructions. Those write targets are workflow rules, not
filesystem access controls. Personal `.claude/settings.local.json` remains
local and unchanged.

On Windows run repository Bash scripts with Git Bash or WSL. Do not assume
that `bash` on PATH is Git Bash. The migration was checked with Git Bash.

## Review findings addressed

- The original root instruction file was about 1,500 lines. The copied
  `AGENTS.md` also referenced nonexistent `.Codex` queue and Markdown agent
  paths. Small entry points now route to shared procedures and policy.
- Four Codex adapters duplicated Claude procedures and omitted Task writer.
  Both hosts now have five adapters referring to the same role files.
- Architect's description prohibited all code, while its procedure allowed
  code within `For: Architect` tasks. The description now states the exception.
- Descriptions containing `For: Coder` or `For: Architect` used unquoted
  YAML colons. Adapter descriptions are now quoted to remain scalar strings.
- Helper referred to five other roles even though there are four, and the
  forced Helper default prevented request-based role switching.
- Architect's self-scheduling exception bypassed triage. Scheduling now
  requires `Thinking:` and `Decision: Will do` regardless of authorship.
- Task resumption could skip an in-progress subtask or an earlier review.
  Resume in-progress work first and wait at the review gate.
- The Task writer could advance the roadmap with untriaged entries still
  pending. An empty queue is now an explicit prerequisite.
- Claude-specific file-tool requirements were copied into Codex. Shared
  procedures now permit the tools provided by each host.
- Stale policy references, the diagram index's authorship-based routing,
  conflicting milestone numbering guidance, and the claim that Cargo enforces
  the allowed-edge list were corrected. Cargo checks cycles; the edge list
  remains a project policy.

This was an instruction/configuration review, not a fresh correctness audit of
the database. The existing task, problems, roadmap statuses and Rust sources
were preserved. Large policy sections were retained in the guide to avoid
silently discarding durability, documentation or review requirements.

## Format references and smoke checks

Codex's [custom agent schema](https://developers.openai.com/codex/subagents)
defines standalone project TOML agents. Its
[instruction discovery](https://developers.openai.com/codex/guides/agents-md)
has a default 32 KiB combined limit. Claude documents the
[AGENTS.md import](https://code.claude.com/docs/en/memory#agentsmd) and
[custom agent format](https://code.claude.com/docs/en/sub-agents).

After restarting each client, ask it to name the five roles, identify the
shared queue paths, and explain who may set a subtask Done. In Claude,
`/context` can confirm the imported memory. In Codex, explicitly request a
read-only Helper subagent to confirm discovery if delegation is needed.
These interactive checks do not require changing the current task.

Migration validation: `bash scripts/check_docs.sh` and `git diff --check`
passed. Python's standard-library parsers accepted all Codex TOML files and
Claude project JSON. Claude frontmatter was checked against its restricted
scalar-field shape, including JSON-compatible quoted descriptions. All five
roles exist for both hosts, their shared procedures exist, and ADR references
in the new documents resolve. `AGENTS.md` is below Codex's default size limit.
Live client discovery was not exercised; restart smoke checks remain as above.
