# txn

The transaction subsystem: transaction lifecycle, two-phase locking, and
the multi-version concurrency control types used for snapshot isolation.

## Architecture

`txn` depends on `storage` (a lock is taken on a physical `Rid`/`PageId`,
and MVCC version chains live alongside the tuples they version) but not on
`catalog` or `sql` (see `docs/adr/0002-crate-splitting.md`): it has no
notion of tables or SQL, only of transactions and the physical records they
touch. `executor` and `engine` sit above it and are the only crates that
drive a `TransactionManager`.

`TransactionManager` owns every transaction's lifecycle end to end:
`begin` opens one against a live `BufferPool` (appending a `Begin` WAL
record), `commit` force-flushes the log up to its `Commit` record before
returning, and `abort` walks its WAL chain backward, undoing each record —
the same `undo_transaction` path `storage::recovery::recover`'s own Undo
pass uses on restart, so `ROLLBACK` and crash recovery really are one
mechanism (`crates/engine/tests/rollback_matches_recovery_undo.rs` proves
this). `write_checkpoint` writes a fuzzy checkpoint so a future recovery's
Analysis pass doesn't have to scan the log from the beginning.

Atomicity and durability are real today, built on exactly that WAL
plumbing; consistency and isolation are much narrower than the word
"transaction" implies — see `docs/adr/0004-acid-scope.md` for precisely
what holds and why. `LockManager` is wired into `TransactionManager` and
the executors as of M10.2 (see Features); `mvcc` still exists here only as
a type, not yet wired into anything.

## Key Components

- `checkpoint` - `write_checkpoint`, writes a fuzzy checkpoint. See
  [checkpoint.MD](src/checkpoint.MD).
- `error` - `TxnError`, errors raised by the transaction subsystem. See
  [error.MD](src/error.MD).
- `isolation` - `IsolationLevel`, the isolation level a transaction runs
  under. See [isolation.MD](src/isolation.MD).
- `lock_manager` - `LockManager`, `LockMode`: grants and releases row and
  table locks under two-phase locking, releasing a transaction's whole
  set at once. See [lock_manager.MD](src/lock_manager.MD).
- `manager` - `TransactionManager`, owns the lifecycle of every
  transaction. See [manager.MD](src/manager.MD).
- `mvcc` - `VersionChain`, `VersionEntry`: the MVCC version chain for a
  single logical row. See [mvcc.MD](src/mvcc.MD).
- `transaction` - `Transaction`, `TransactionState`: a single unit of work
  and its position in the 2PL protocol. See
  [transaction.MD](src/transaction.MD).

## Features

`BEGIN`/`COMMIT`/`ROLLBACK` work today with real atomicity and durability,
and checkpointing is wired into `engine::Database` on a byte-growth
threshold. Isolation is enforced by two-phase locking rather than by the
absence of concurrency: `engine::Database` runs different sessions'
statements concurrently on a worker pool (`docs/ROADMAP.md`'s M10.2), and
every reader and writer holds its table/row locks until its transaction
ends (see below), so one transaction's uncommitted writes are never
visible to, or overwritten by, another. What 2PL alone does not give is a
repeatable-read or snapshot guarantee - a transaction that reads the same
row twice, releasing and reacquiring the lock in between, can still see it
change - until MVCC (`docs/ROADMAP.md`'s M10.3) lands.

`LockManager::lock`/`lock_table`/`release_all` are implemented, with
deadlock detection (there is no `unlock` method - strict two-phase locking
releases a transaction's whole lock set at once, via `release_all`, never
one lock at a time). `TransactionManager::commit`/`abort` both call
`release_all`, and `executor::SeqScanExecutor`/`IndexScanExecutor`/
`InsertExecutor` take the locks it grants (`docs/ROADMAP.md`'s M10.2).
`VersionChain::visible_version` is still `todo!()` - that's M10.3, MVCC for
snapshot isolation, wired into every read and write path once it lands.
See `docs/ROADMAP.md` and `docs/adr/0004-acid-scope.md`.

## Dependencies

Workspace: `common`, `storage` (locks and log records are keyed by
`storage`'s `Rid`/`PageId`/`TxnId`, and `write_checkpoint`/`begin`/`commit`/
`abort` all append to a live `BufferPool`'s WAL). External: `thiserror`,
for `TxnError`. Dev-only: `tempfile` and `test-support`
(`crates/test-support/README.md`) for its shared pool-opening fixture.

## Configuration

None as a runtime knob of this crate itself — `TransactionManager::new`
takes the highest `TxnId` recovery observed at startup, and
`Database::maybe_checkpoint` (in `engine`) decides *when* to call
`write_checkpoint` based on `common::DbConfig::checkpoint_byte_threshold`,
but that threshold isn't part of this crate's own state.

## Testing

`tests/lifecycle.rs` exercises `TransactionManager::begin`/`commit`/`abort`
and `write_checkpoint` against a real `BufferPool`, proving the wiring —
not just that the types compile. `tests/lock_manager.rs` exercises
`LockManager` directly: shared/exclusive conflicts, upgrade-in-place,
blocking and waking via `release_all`, and deadlock detection choosing
exactly one victim. `tests/smoke.rs` is the minimum-viable
compile-and-construct check. Run just this crate with:

```sh
cargo test -p txn
```
