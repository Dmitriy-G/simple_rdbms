# CLAUDE.md

@AGENTS.md

Claude role adapters are in `.claude/agents/`. Shared policy and procedures
are in `docs/agent-guide.md` and `docs/agents/`; read the relevant sections.
The list below stays here because `scripts/check_docs.sh` reads it directly.

## Known scaffolding

Code that exists but cannot be reached yet, so it should not be mistaken
for working capability. Each is named with the milestone that finishes
it — this was requested in an earlier roadmap task and never landed, and
belongs here rather than in `docs/ROADMAP.md` since this is the file a
fresh session reads first.

**A bullet here is deleted by the work that makes it false, in the same
task.** When a task closes a piece of scaffolding, the Task writer adds
"remove the `CLAUDE.md` scaffolding bullet this closes" to the subtask
that closes it, exactly as a `.MD` update ships with its `.rs` — and
since this file is Architect-owned, a Coder subtask discharges that by
naming the bullet in its reply so the deletion happens alongside the
review. A list of things that do not work is only useful while every
line on it is still true; one stale bullet tells a fresh session that a
shipped capability is missing, which is worse than not listing it at all.

- `catalog::Column::nullable` — parsed, persisted, plumbed through the
  binder, never enforced (M15).
- `common::SqlState::NOT_NULL_VIOLATION` — defined, never raised (M15).
- `common::SqlState::UNIQUE_VIOLATION` — defined, never raised (M16); the
  B+tree still permits duplicate keys.
- `storage::btree::BTreeIndex::delete` — the method exists
  (`crates/storage/src/btree.rs:663`); its body is `todo!()`, and M14
  changes its signature to `(txn_id, key, rid)` as well as filling it in.
- `catalog::Catalog::drop_table` — the method exists
  (`crates/catalog/src/catalog.rs:168`); its body is `todo!()` and nothing
  in the tree calls it, since no grammar produces `DROP TABLE` (M26).
- `executor::NestedLoopJoinExecutor` — exists and is wired into the
  executor factory; `init` and `next` are both `todo!()` (M23.1).
  `planner::LogicalPlan::Join`/`PhysicalPlan::NestedLoopJoin` already
  exist as the node kinds it would run, but nothing in `sql`'s grammar
  can produce them yet — `FROM` accepts exactly one table (M23.1).
- `txn::VersionEntry::end_ts` — the field exists and
  `VersionChain::is_globally_visible` reads it
  (`crates/txn/src/mvcc.rs:36-37`), but no caller ever sets it: every
  entry is created with `end_ts: None`
  (`crates/txn/src/version_store.rs:31`) and the store has no mutator
  that changes it, because nothing supersedes a version until `UPDATE`
  and `DELETE` exist (M14).
- `txn::VersionStore::is_visible` and the
  `VersionChain::visible_version` it calls — implemented, but reachable
  only from `crates/txn/tests/`. The live path every executor uses is
  `is_visible_to`/`visible_version_for`, which also treats a row's own
  writer as able to see them. M14 deletes the pair unless the ADR 0004
  revisit finds a reader with no transaction id.

Milestone numbers are positions in the roadmap, not stable names. Inserting
or removing a milestone renumbers subsequent entries and all their references
in the same change. So an `Mx` written anywhere outside `docs/ROADMAP.md` — a
bullet above, a sentence in a `.MD`, a `// TODO(Mx):` marker — is a
reference that renumbering can silently invalidate. **Resolve one
against `docs/ROADMAP.md` before trusting it**, and when the milestone it
names is Done but the work it describes plainly is not, the marker is
stale: retarget it to the milestone that will actually do the work, in
whatever task next has that file open. A marker naming a Done milestone
reads as "already shipped" to every session after it.
