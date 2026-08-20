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
