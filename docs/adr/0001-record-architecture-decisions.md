# ADR 0001: Record architecture decisions

Date: 2026-08-20

Status: Accepted

## Context

This project will accumulate decisions (crate boundaries, storage formats,
concurrency control strategy) that are expensive to reverse and not
obvious from reading the code alone. Without a record, future contributors
(including the current ones, later) re-litigate settled questions or
violate constraints they didn't know existed.

## Decision

We will record architecturally significant decisions as Architecture
Decision Records (ADRs) in `docs/adr/`, numbered sequentially, using the
template in `docs/adr/template.md`. A decision is significant if reversing
it would require a nontrivial rewrite, or if it constrains later decisions
(e.g. the crate-splitting decision in ADR 0002).

## Consequences

Every future significant decision gets a numbered file instead of living
only in a PR description or commit message. Superseding a decision means
adding a new ADR that says so and updating the old one's status, not
deleting it — the history of *why* is as valuable as the current answer.

Revising is not superseding, and the two are handled differently. An ADR
is **superseded** when the decision it records is reversed or replaced: a
new ADR says so, the old one's status becomes `Superseded by ADR NNNN`,
and both stay readable. An ADR is **revised in place** when the decision
still stands but the facts it states about the code have changed
underneath it — most often because the ADR named a milestone as its own
revisit trigger and that milestone landed. A revision keeps the number
and the status, adds a `Revised: YYYY-MM-DD — what changed` line under
`Date:`, and says in the Context what the previous version claimed and
why it stopped being true, so the history survives the edit.
`docs/adr/0004-acid-scope.md` is the worked example: M10 made its
isolation section false without reversing anything it decided.

A number belongs to a file, not to a plan. Do not cite an ADR by number
before it exists — numbers are assigned in creation order, and a
reservation that is only in someone's head gets handed to the next ADR
written.
