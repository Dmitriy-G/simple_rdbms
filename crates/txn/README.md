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
the executors as of M10.2, and `mvcc`/`version_store` are wired into the
same path as of M10.3 (see Features).

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
- `version_store` - `VersionStore`: the shared, process-wide map from
  `Rid` to `VersionChain` that backs snapshot-isolation reads. See
  [version_store.MD](src/version_store.MD).

## Features

`BEGIN`/`COMMIT`/`ROLLBACK` work today with real atomicity and durability,
and checkpointing is wired into `engine::Database` on a byte-growth
threshold. `engine::Database` runs different sessions' statements
concurrently on a worker pool (`docs/ROADMAP.md`'s M10.2). The isolation a
transaction gets today depends on which side of a read/write pair it is
on, not on a single mechanism for every transaction: every writer still
takes table/row locks under two-phase locking and holds them until its
transaction ends, so one transaction's uncommitted writes are never
overwritten by another and two writers never race on the same row; a
reader running under `IsolationLevel::SnapshotIsolation` takes no
table or row locks at all and instead consults `VersionStore` to see
exactly the rows committed at or before its own snapshot's `read_ts` (plus
its own uncommitted writes), which is what actually gives it a repeatable,
non-changing view of the database across the whole transaction - something
2PL alone cannot, since releasing and reacquiring a row lock lets the row
change underneath a reader in between. There is no write-write conflict
detection beyond the row locks writers already take: today `INSERT` is the
only write path, so two writers can never target the same existing `Rid`,
which is the only reason this is harmless rather than a gap - it stops
being harmless the moment `UPDATE`/`DELETE` (`docs/ROADMAP.md`) land.

`LockManager::lock`/`lock_table`/`release_all` are implemented, with
deadlock detection (there is no `unlock` method - strict two-phase locking
releases a transaction's whole lock set at once, via `release_all`, never
one lock at a time). `TransactionManager::commit`/`abort` both call
`release_all`, and `executor::SeqScanExecutor`/`IndexScanExecutor`/
`InsertExecutor` take the locks it grants (`docs/ROADMAP.md`'s M10.2).
`VersionChain::visible_version`/`visible_version_for` are implemented and
`VersionStore` (`mvcc.MD`, `version_store.MD`) wraps them behind a shared,
`Rid`-keyed map: `TransactionManager::commit`/`abort` call
`VersionStore::commit_versions`/`abort_versions`
(`crates/txn/src/manager.rs`), `InsertExecutor` records every inserted
row's version, and `SeqScanExecutor`/`IndexScanExecutor` consult
`VersionStore::is_visible_to` under `SnapshotIsolation` instead of locking
(`docs/ROADMAP.md`'s M10.3). See `docs/ROADMAP.md` and
`docs/adr/0004-acid-scope.md`.

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
exactly one victim. `tests/mvcc.rs` exercises `VersionChain`/`VersionEntry`
directly: visibility around `begin_ts`/`end_ts`, uncommitted entries never
being visible to another transaction, and a reader seeing its own
uncommitted entry. `tests/version_store.rs` exercises `VersionStore` the
same way, but through its shared, `Rid`-keyed map rather than a single
chain: recording, committing and aborting insert versions, and
`is_visible`/`is_visible_to` agreeing on committed rows while
`is_visible_to` alone shows a reader its own uncommitted insert.
`tests/smoke.rs` is the minimum-viable compile-and-construct check. Run
just this crate with:

```sh
cargo test -p txn
```
