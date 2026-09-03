# Archived task specs

One file per finished `.claude/task.MD`, named
`<milestone>-<slug>.md` — for example `M10.2-two-phase-locking.md`.

The Task writer moves the live task here once every subtask in it is
done, before writing the next one. The point is that a reviewer looking
at a past milestone can still read the spec the code was written
against: `.claude/task.MD` only ever holds the task in progress, and a
commit message is not a specification.

These files are history. Do not edit an archived task to match what the
code ended up doing — if the code diverged from the spec, that belongs in
`../../.claude/investigations.md` or in the milestone's `docs/ROADMAP.md`
entry, where someone will actually look for it.