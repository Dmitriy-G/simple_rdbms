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
| `.codex/profile-templates/*.config.toml` | Versioned templates for machine-local CLI profiles |
| `.codex/config.toml` | Enables spawned Codex custom agents |
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

Requests map to these roles:

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
All five are provided, including `task-writer`, and each pins the model of a
spawned subagent. They do not configure `codex --profile`: that command starts
the main session with `$CODEX_HOME/<name>.config.toml`. Codex ignores profile
definitions in project `.codex/config.toml`, so the versioned files under
`.codex/profile-templates/` must be copied to `$CODEX_HOME` on each machine.

Profile names describe their model and reasoning configuration, not a project
role. That separation is deliberate: `.claude/problems.md` uses `Thinking:`
to choose the model for its next action, while `.claude/task.md` contains
only implementation-ready Coder work. The available templates are:

| CLI profile | Model | Reasoning |
| --- | --- | --- |
| `astra-xhigh` | `gpt-6-astra` | `xhigh` |
| `luna-medium` | `gpt-5.6-luna` | `medium` |
| `sol-medium` | `gpt-5.6-sol` | `medium` |
| `sol-xhigh` | `gpt-5.6-sol` | `xhigh` |

The shared tier mapping is:

| Model tier | Thinking | Codex minimum | Claude minimum |
| --- | --- | --- | --- |
| Light | 1–3 | Luna with medium reasoning | Sonnet |
| Standard | 4–7 | Sol with medium reasoning | Sonnet |
| Advanced | 8–10 | Astra with xhigh reasoning | Opus |

These are minimums. Advanced is reserved for investigation and review; it
never appears in `.claude/task.md`. A stronger model within the same
responsibility may work a lower tier. Light and
Standard currently share the same Claude minimum because this repository
does not version a lighter Claude adapter; keeping the tiers distinct still
preserves the estimate and permits a cheaper mapping later without changing
the queue format. Model names belong only in this setup document. Queue and
task policy use the stable tier names.

For example, `.codex/profile-templates/sol-medium.config.toml` installs as
`$CODEX_HOME/sol-medium.config.toml`; then `codex --profile sol-medium` starts
the main session at Standard tier. The profile selects configuration, not a
role prompt: the request is still routed through `AGENTS.md`. There is no
Advanced Coder task: Astra investigates and rewrites the problem until the
remaining implementation can be rated for Luna or Sol. Spawn a custom agent
only when delegation is wanted. New sessions are needed to load profile or agent changes; project
configuration also depends on the host trusting the checkout.

| Codex agent | Pinned model | Reasoning |
| --- | --- | --- |
| `architect` | `gpt-6-astra` | `xhigh` |
| `coder` | `gpt-5.6-sol` | `medium` |
| `helper` | `gpt-5.6-luna` | `medium` |
| `milestone-reviewer` | `gpt-6-astra` | `xhigh` |
| `task-writer` | `gpt-5.6-sol` | `xhigh` |

The role agents are convenience defaults. The `coder` adapter supplies
Standard implementation and the `architect` adapter supplies Advanced
investigation. A Light Coder task may run in a Luna main session. A narrowed
Architect investigation may run in Sol or Luna after a stronger pass lowers
its Thinking level. A task worker checks `Model tier:` before changing a
status marker; if its session is below the stated tier or cannot identify its
tier, it stops and reports the required tier. It never changes roles or
delegates merely to satisfy that gate.

## Responsibility matrix

This table is the operational model-selection policy. `Next:` applies to
problem entries; a dash means the activity does not consume a problem entry.

| Activity | Role | Thinking / tier | Next | Codex | Claude | Output |
| --- | --- | --- | --- | --- | --- | --- |
| Simple project question or explanation | Helper | Light | — | Luna, medium | Sonnet, medium | Read-only answer |
| Mechanical correction in Architect-owned prose | Architect | 1–3 / Light | Investigate | Luna, medium | Sonnet, medium | Corrected prose; settled entry removed if one existed |
| Whole `.claude/problems.md` review and triage | Architect | Advanced session | — | Astra, xhigh | Opus, xhigh | Thinking, Next and Decision on every entry; then human approval |
| Deep or open-ended problem investigation | Architect | 8–10 / Advanced | Investigate | Astra, xhigh | Opus, xhigh | Rewritten problem, evidence, recommendation and new Thinking |
| Narrow follow-up investigation after decomposition | Architect | 4–7 / Standard | Investigate | Sol, xhigh | Sonnet, medium | Further-refined problem and new Thinking |
| Task writing from implementation-ready problems | Task writer | 1–7 input | Implement | Sol, xhigh | Opus, xhigh | `For: Coder` task with Light or Standard tier |
| Simple mechanical implementation or crate-doc fix | Coder | 1–3 / Light | Implement | Luna, medium | Sonnet, medium | One reviewed task subtask |
| Routine coding, tests and executable configuration | Coder | 4–6 / Standard | Implement | Sol, medium | Sonnet, medium | One reviewed task subtask |
| Public-API or cross-crate implementation with a settled design | Coder | 7 / Standard | Implement | Sol, xhigh | Sonnet, medium | One reviewed task subtask |
| Architecture decision, ADR, roadmap or workflow design | Architect | 8–10 / Advanced | Investigate | Astra, xhigh | Opus, xhigh | Durable decision plus refined or closed problem |
| Whole-milestone functional review | Milestone Reviewer | Advanced session | — | Astra, xhigh | Opus, xhigh | Gating/non-gating findings and milestone verdict |

If an intended implementation still rates `8`–`10`, it is not ready for
the Task writer. The Architect investigates or decomposes it again. Astra
never executes `.claude/task.md`, and Sol never invents the missing design
while writing a task.

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
- Architect never writes production code or works `.claude/task.md`.
  Advanced problems remain in the queue until investigation reduces the
  remaining implementation to Light or Standard.
- Descriptions containing `For: Coder` or `For: Architect` used unquoted
  YAML colons. Adapter descriptions are now quoted to remain scalar strings.
- Helper referred to five other roles even though there are four, and the
  forced Helper default prevented request-based role switching.
- Scheduling requires `Thinking: 1`–`7`, `Next: Implement` and
  `Decision: Will do` regardless of authorship.
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
