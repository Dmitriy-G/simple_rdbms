# ADR 0018: An abort is not a quiesce point

Date: 2026-09-09

Status: Accepted

## Context

**This section describes the tree as it stood when M10's review filed
P-63**; its citations are historical, and the Decision below is what the
code should do.

ADR 0016 removed a stall from the checkpoint path and closed with the
claim that the rule generalizes: "the next component to acquire a
long-held lock inherits it without a third ADR." The abort path did not
acquire a long-held lock afterwards — it had been holding one all along,
and nobody had looked.

`EngineShared::abort_txn` took the `EngineShared.txn_manager` mutex and
called `TransactionManager::abort`, which ran
`storage::recovery::undo_transaction` with the mutex still held. Undo
walks a transaction's whole undo chain backwards, restoring each record's
before-image through `BufferPool::stamp_write`, which fetches the page it
is restoring. A fetch that finds no free frame evicts a victim and flushes
it inline: a write to the database file through the double-write buffer,
then a device `sync`. So an abort held, across an unbounded number of page
writes and at least one `fsync`, the same mutex `txn_for_statement`,
`handle_begin` and `commit_txn` each need for microseconds to start or
finish any statement on any session.

The blast radius was wider than a client-issued `ROLLBACK`. Two paths
abort on a session's behalf with nobody asking — `expire_idle_session`
when a transaction sits idle past `DbConfig::idle_in_transaction_timeout_ms`,
and `disconnect_session` when a connection closes with a transaction
open — and both went through the same `abort_txn`. A rollback is also not
a rare event the way a checkpoint is: every failed statement in autocommit
takes it.

What made this more than a mechanical repeat of ADR 0016 is that the
checkpoint's two phases share only a token, while an abort's phases share
the transaction itself. Splitting it forces a question the checkpoint
never raised: **while the undo runs with the lock released, is the
transaction still active?** Its answer moves three watermarks, and one of
them decides whether the log records the undo is reading still exist.

## Decision

**No lock that a statement needs may be held across a transaction's undo.**
`TransactionManager::abort` splits into three calls, and the transaction
manager belongs to the first and the last only:

- **Phase one, `TransactionManager::begin_abort`, needs the manager.** It
  confirms the transaction is active and not already aborting, marks it
  `TransactionState::Aborted`, emits the abort `warn!`, and returns a
  `PendingAbort` carrying the transaction's id and the `last_lsn` its undo
  chain starts from, read under the lock.
- **Phase two, `PendingAbort::undo`, needs nothing but the buffer pool.**
  It runs `storage::recovery::undo_transaction`, which appends every `Clr`,
  restores every before-image, and closes with the `Abort` and `End`
  records.
- **Phase three, `TransactionManager::finish_abort`, needs the manager
  again.** It removes the transaction from the active set, calls
  `VersionStore::abort_versions` and `prune`, releases every lock through
  `LockManager::release_all`, prunes the lock manager's finished set, and
  increments `transactions_aborted_total`. Its failure counterpart is
  `cancel_abort`, which puts the transaction back to `Growing` and leaves
  it active and locked, so an undo that failed can be retried rather than
  stranding the transaction as permanently un-abortable — which is the
  behaviour the single-call `abort` already had.
- **The engine drops the lock between them.** `EngineShared::abort_txn`
  takes `txn_manager` for phase one, releases it, and retakes it for phase
  three.

`abort` stays as the three phases in sequence, for callers that hold the
manager exclusively for other reasons and have no lock to release —
`crates/txn/tests/lifecycle.rs` and
`crates/executor/tests/index_maintenance.rs` — exactly as `write_checkpoint`
stayed after ADR 0016. The engine no longer calls it.

### The transaction stays in the active set for the whole undo

This is the decision P-63 asked for, and it is not symmetric: leaving
early is unsafe, staying late is merely conservative. Three watermarks are
computed from the active set, and each answers differently.

- **`earliest_active_begin_lsn` decides this, and it decides it alone.**
  It bounds how far a checkpoint may truncate the write-ahead log
  (`docs/adr/0011-segmented-write-ahead-log.md`), and undo reads log
  records from `last_lsn` back to the transaction's `Begin`. If the
  aborting transaction left the active set before its undo, a checkpoint
  running concurrently in the gap would compute a bound that ignores it
  and could truncate away the very records the undo is walking. That is
  data loss on the ordinary rollback path, and it settles the question by
  itself.
- **`oldest_active_txn_id` feeds `LockManager::prune_finished`.** The
  aborting transaction still holds every lock it took until phase three,
  so it must not fall below that watermark while the undo runs. Leaving
  early would let a concurrent commit prune the bookkeeping of a
  transaction that is still a live lock holder.
- **`oldest_active_read_ts` feeds `VersionStore::prune`.** Staying keeps
  this watermark low, which only makes pruning more conservative, so it is
  safe either way. It is worth naming because M10.4's pruning is the part
  a reader would expect to be the constraint, and it is the one watermark
  that is not.

Staying costs what conservatism costs: while a long undo runs, no version
chain older than the aborting transaction's snapshot can be pruned and no
log segment below its `Begin` can be truncated. Both resume the moment
phase three runs. That is the same class of hold an ordinary long-running
transaction already imposes, and an aborting transaction is exactly that
until its undo finishes.

### Why the gap is safe

Four things could go wrong while the lock is down and none of them can:

- **No second thread can undo the same transaction.** `begin_abort` marks
  the transaction `TransactionState::Aborted` under the lock and refuses a
  transaction already so marked with `TxnError::AbortInProgress`. The
  engine serializes a session's statements behind its `SessionState` mutex
  anyway, so this is defence in depth rather than the primary guard — but
  it is the guard that would catch a fifth caller of `abort_txn` written
  later without that knowledge.
- **A concurrent `COMMIT` cannot observe a half-undone transaction.**
  Before this change, being in the active set was proof a transaction
  could still be committed. It is not any more, so `commit` makes the same
  check and raises the same `AbortInProgress`. This is the one place the
  split changes an existing method rather than adding one, and skipping it
  would let a commit force-flush a `Commit` record for a transaction whose
  before-images are already being restored.
- **No other writer can see the table mid-undo.** `release_all` is in
  phase three, after the undo, so the aborting transaction holds its
  exclusive table locks throughout — `docs/adr/0017-create-index-is-a-locked-writer.md`'s
  discipline is what makes this hold. Readers take no locks and see the
  aborting transaction's rows as uncommitted, which is what they were
  before the undo started and what they must stay.
- **`abort_versions` reads `next_ts` later than it used to, and that is
  still correct.** Its contract is that the timestamp it stamps is one
  every transaction that could be holding the undone row's bytes has
  finished by. That follows from every active transaction having a
  `read_ts` below whatever `next_ts` currently is — a property true at any
  moment, not only at the moment the abort began. Reading it in phase
  three is therefore no weaker; it is a larger number bounding a larger
  set.

The alternative considered was **leaving the transaction in the active set
but exposing an "undoing" flag every watermark skips**, so that a long
rollback does not hold back pruning or truncation. It removes the
conservatism named above, and it reintroduces exactly the hazard the first
bullet describes: `earliest_active_begin_lsn` must not skip it, so the
flag would need a per-watermark exception list. Rejected as more machinery
for a cost that is bounded by the undo's own duration.

## Consequences

`txn`'s public API gains `PendingAbort`, `TransactionManager::begin_abort`,
`finish_abort` and `cancel_abort`, and `TxnError` gains
`AbortInProgress(u64)`, which maps to `common::Error::Internal` alongside
`LockAfterUnlock` and `UnknownTransaction` since all three mean this
crate's own bookkeeping was violated rather than anything a client's SQL
caused.

`TransactionState::Aborted` becomes the first state anything actually
writes. The enum was documented as a transaction's position in the 2PL
protocol and never set past `Growing`; it now means "this transaction's
undo is running or has run", which is what `transaction.MD` already
claimed for it.

`crates/engine/tests/dispatch_never_blocks.rs`'s
`a_stalled_rollback_does_not_block_an_unrelated_session` is the regression
test: with a rollback's undo parked mid-eviction at the device, an
unrelated session's `Connect`, `BEGIN` and `COMMIT` must all complete
within the same tight bound ADR 0016's probe uses. `BEGIN` and `COMMIT`
are the two statements that take this mutex, so they are the whole
assertion.

It is a narrower probe than ADR 0016's, which also runs a `SELECT`, and
the reason is worth recording because it will come up again. While any
device write is stalled, no session anywhere can evict a buffer pool
frame: `BufferPool::flush_pages` holds `BufferPool.flush_sequence` across
its whole double-write batch. A probe that reads a table whose page is not
resident therefore blocks on the buffer pool regardless of what the
transaction manager is doing, which is what a first version of this test
demonstrated — `Connect` and `BEGIN` returned well inside the bound and
the `SELECT` timed out. The checkpoint test's `SELECT` passes only because
its default-size pool never needs to evict. That serialization is a
property one layer below this ADR's and is not changed by it.

The rule ADR 0014 stated for the catalog and ADR 0016 for the checkpoint
is now stated for the third time, on a path that inherited it and did not
get it. That is the finding worth carrying forward: a rule of the form
"never hold *this* lock across I/O" is not discharged by fixing the caller
that revealed it, because the lock has other callers. The check that
generalizes is to enumerate every site that takes the lock and ask what
each does while holding it — and `EngineShared`'s are `begin`, `commit`,
`begin_abort`/`finish_abort`, `get`, `active_snapshot` and the two
`test-util` accessors, which is a short enough list to read in full.
