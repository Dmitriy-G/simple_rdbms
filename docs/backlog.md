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

## `clippy.toml`'s `msrv` understates the workspace's `rust-version`

`Cargo.toml` requires Rust 1.90 since M13.2 pinned the container's builder
image, but `clippy.toml`'s `msrv` is still `1.85`, so clippy is more
conservative than it needs to be about the idioms it suggests. Not being
done because raising it makes `collapsible_if`'s let-chain rewrite and
`manual_is_multiple_of` fire under `-D warnings` at five sites, three of
them in `crates/storage/src/btree.rs` and one in
`crates/storage/src/buffer.rs`, which puts both crash-injection sweeps on
a change with no behavioural difference at all. Nothing is miscompiled and
no lint is suppressed incorrectly: `cargo clippy -D warnings` is clean as
the file stands. The fix, whenever a storage task is already paying for
the sweeps, is to rewrite the five sites and bump the `msrv` in the same
change.

Created: 2026-09-11

Importance: 🟢 Low

Effort: 8 SP

Thinking: 4

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
