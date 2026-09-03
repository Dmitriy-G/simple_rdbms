# Archived task specs

One file per finished `.claude/task.md`, named `<milestone>-<slug>.md` —
for example `M10.2-two-phase-locking.md`.

The Task writer moves the live task here once every subtask in it is
done, and before writing the next one.

**This archive is the only copy.** `.claude/task.md` is gitignored and
holds exactly one task at a time, so a spec that is not archived before
the next task overwrites it is gone — not stale, not out of date, gone.
The same goes for the problems consumed into it: an entry is deleted from
`.claude/problems.md` when it becomes a subtask, which makes the archived
task the sole record that the problem was ever found. Archiving is not
tidying up after the work; it is the last step of the work.

That is also what makes a past milestone reviewable. A reviewer can read
the spec the code was written against, which a commit message is not.

These files are history. Do not edit an archived task to match what the
code ended up doing — if the code diverged from its spec, that belongs in
`../../.claude/problems.md` as a finding, or in `docs/adr/**` and the
milestone's `docs/ROADMAP.md` entry if it is a decision worth keeping.
