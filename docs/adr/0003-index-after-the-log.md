# ADR 0003: Move the B+tree index behind the WAL and recovery

Date: 2026-08-22

Status: Accepted

## Context

The original roadmap put the B+tree index at M5, directly after SQL support
(M4) landed: "answering point/range lookups without a full scan" read as
the next most valuable feature to build, and nothing about it seemed to
depend on the write-ahead log slated for M6.

Implementing M5 as originally planned surfaced two concrete data-loss bugs
first, though (see the current M5 in `ROADMAP.md`): `DiskManager::allocate_page`
wrote a `page_count` into the page-0 header durably while the new page's own
initialization stayed dirty in the buffer pool, and dirty pages only ever
reached disk in `Database::close`/`Drop`, neither of which runs on a kill
signal. Fixing those was a stopgap — deriving `next_page_id` from the file's
own length and syncing after every mutating statement — not a real
write-ahead log. That stopgap is what "M5" now names.

With the stopgap in place, the question became: build the B+tree next, or
build the WAL and recovery next? The B+tree's own writes are exactly the
kind of multi-page operation the stopgap does not protect. A leaf split
touches the new leaf, the parent's inserted separator key, and often a
sibling pointer on a neighboring leaf — three pages that need to reach disk
as a unit. The heap tolerates a half-finished write today only because an
all-zero page was made to decode as a valid, empty page (see
`heap::NO_NEXT_PAGE`); there is no equivalent trick for a half-finished
B+tree split, which can leave a key routed to the wrong child or a leaf
unreachable from the root — silent, wrong answers rather than a page that's
merely short some rows. Building the index against the pre-WAL page-write
path would also mean rewriting its recovery story from scratch once the WAL
did land, since its split protocol would need to log its own before/after
images (physiological logging) to be redo/undo-able at all.

## Decision

Sequence the write-ahead log (M6) and ARIES-style recovery (M7) ahead of the
B+tree index, which moves to M9. `Database::sync`'s per-statement flush
(M5) remains the durability story for heap-only tables until the WAL lands;
the index is designed against a storage layer that already knows how to
log and replay a multi-page operation, instead of being designed once and
then reworked.

## Consequences

Point and range lookups keep paying for a full sequential scan through M6,
M7, and M8 (transactions) — those queries stay correct, just not fast, in
the interim, which is an acceptable trade for not having to redesign the
index's crash-recovery story mid-flight. It also puts M8 (single-transaction
atomicity) and M10 (concurrent isolation) ahead of the index in the roadmap;
that's fine, since the index sits below both the same way the heap already
does. The B+tree's split/merge protocol, when it's built, should be
designed with the WAL's redo/undo record format in mind from the start
rather than bolted on afterward — that's the whole point of this ordering.
