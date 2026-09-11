# ADR 0016: A checkpoint is not a quiesce point

Date: 2026-09-08

Status: Accepted

## Context

**This section describes the tree as it stood when M10.2's P-54 was
finished**; its citations are historical, and the Decision below is what
the code should do.

`txn::write_checkpoint` took `&mut TransactionManager`, so its caller held
whatever lock guarded the transaction manager for the whole of it. In the
engine that lock is `EngineShared.txn_manager`, a plain `Mutex`, and the
whole of it included a data-page flush and a device `fsync`: the function
ended by writing `last_checkpoint_lsn` into the page-0 header through an
ordinary logged transaction and then forcing that page out with
`flush_page(PageId(0))` followed by `sync()`.

Every statement needs that same mutex. `EngineShared::txn_for_statement`
takes it to begin an autocommit transaction and `handle_begin` takes it
for an explicit `BEGIN`, each for microseconds. A checkpoint therefore
stopped every session in the database for as long as one page write and
one `fsync` took — and `maybe_checkpoint`, which runs on whichever worker
thread committed the statement that pushed the log past
`DbConfig::checkpoint_byte_threshold`, is the common path. `Database::close`
is the rare one.

This was found the hard way. P-54 moved the `Checkpoint` and
`BestEffortFlush` message arms off the engine's dispatch thread and onto
the worker pool, and its regression test tried to prove the property
anyone would expect from that change: while a checkpoint's flush is
stalled, an unrelated session's `SELECT` still completes. The test hung —
not intermittently, but every time and unboundedly — and the hang was
read at first as a defect in the P-54 change itself. It was not: the
dispatch thread was free, and the probe was parked on this mutex, one
layer below the one that had just been fixed. The module's own
documentation had claimed since it was written that a fuzzy checkpoint
"deliberately does not flush data pages or block any other activity"; the
first half was true of the checkpoint's own semantics and the second half
was false in the engine that called it.

The general shape is the same one ADR 0014 settled for the catalog: a lock
taken for a short logical operation, held across I/O whose duration
nothing bounds. The question this ADR answers is the checkpoint's half of
it — **what may a checkpoint hold while it makes itself durable?**

## Decision

**No lock that a statement needs may be held across a checkpoint's data
page I/O.** Concretely, a checkpoint runs in two phases, and the
transaction manager belongs to the first only:

- **Phase one, `txn::write_checkpoint_record`, needs the transaction
  manager.** It appends `CheckpointBegin`, snapshots the Active
  Transaction Table and the Dirty Page Table, appends `CheckpointEnd`,
  flushes the log, then begins, writes and commits the header transaction
  that stamps `last_checkpoint_lsn` into page 0, and computes the log
  truncation bound. It returns a `PendingCheckpoint` carrying that bound,
  the `CheckpointBegin` LSN and the checkpoint's start instant.
- **Phase two, `txn::finish_checkpoint`, needs nothing but the buffer
  pool.** It flushes page 0, syncs the device, truncates the log below the
  bound, and records the duration metric and the completion `info!`.
- **The engine drops the lock between them.**
  `EngineShared::checkpoint_and_flush` and `EngineShared::maybe_checkpoint`
  take `txn_manager` for phase one only.

Phase one still does log I/O under the lock — the two record appends and
`flush_log_all`, plus the header transaction's own commit flush. That is
deliberate and is not the same problem: every ordinary `COMMIT` already
flushes the log under this exact mutex (`EngineShared::commit_txn`), so a
checkpoint's log work costs what one more commit costs. What made a
checkpoint different was the *data file* — a page write plus a full
`sync()` of the database file, which is unbounded relative to anything a
statement does. Narrowing the lock further, so that no log flush happens
under it either, is a larger change to how commits serialize and is not
part of this decision.

**Why phase two is safe outside the lock.** Three things could go wrong
and none of them can:

- **The truncation bound cannot go stale.** It is
  `min(the lowest recLSN in the Dirty Page Table, the earliest active
  transaction's begin LSN, this checkpoint's own begin LSN)`, all measured
  in phase one. A transaction that begins during the gap gets a `Begin`
  LSN higher than every LSN phase one saw; a page dirtied during the gap
  gets a recLSN that is likewise later. Neither can pull the minimum down,
  so truncating to a bound computed a moment earlier deletes strictly less
  than a freshly computed one would.
- **The header can never advance past a checkpoint whose records are not
  durable.** `flush_log_all` in phase one makes `CheckpointBegin` and
  `CheckpointEnd` durable *before* the header is stamped, and the header
  write is itself an ordinary logged page mutation, redone by recovery
  like any other (`docs/agent-guide.md`, "The page-0 header is versioned and
  logged"). Phase two's flush only makes an already-logged value reach its
  home location sooner. A crash between the phases loses nothing:
  recovery starts from the previous checkpoint's LSN and redoes the header
  write from the log.
- **Two checkpoints overlapping in phase two are harmless.** The header
  value is written under the lock in phase one, so header updates are
  ordered by LSN and can never go backwards; phase two only flushes
  whatever page 0 currently holds, which is that value or a newer one.
  `flush_page` is internally serialized by the buffer pool's flush
  sequence, and `truncate_log_below` called with the older, smaller bound
  after the newer one has run removes nothing extra.

The alternative considered was **giving the checkpoint its own mutex** so
that at most one runs at a time and it takes `txn_manager` only for the
ATT snapshot. It solves the same stall, but it adds a second lock and a
second ordering rule to reason about, and it does not remove the need for
the phase split — the page flush would still have to sit outside the
transaction manager's lock to stop the stall. It was rejected as strictly
more machinery for the same result. Concurrent checkpoints remain
possible and remain merely redundant, exactly as `runtime.MD` already
described them.

## Consequences

`txn`'s public API gains `PendingCheckpoint`, `write_checkpoint_record`
and `finish_checkpoint`. `write_checkpoint` stays, as the two phases in
sequence, for callers that hold the transaction manager exclusively for
other reasons — `crates/txn/tests/lifecycle.rs` — and the engine no longer
calls it.

`crates/engine/tests/dispatch_never_blocks.rs`'s
`a_stalled_checkpoint_does_not_block_an_unrelated_session` is the
regression test, and it is now the strong form: with a checkpoint's page-0
write stalled at the device, an unrelated session's `Connect`,
`table_names()` and a real `SELECT` must all complete within the same
tight bound the P-14 probe uses. Before this change the `SELECT` hung
indefinitely while the other two passed, which is precisely the difference
between the dispatch thread being free and the database being usable.

The rule generalizes, and `docs/agent-guide.md`'s invariant list now carries it: an
engine-level lock that a statement takes may not be held across a data
page flush or a device sync. ADR 0014 said it for the catalog, this ADR
says it for the transaction manager, and the next component to acquire a
long-held lock inherits it without a third ADR.
