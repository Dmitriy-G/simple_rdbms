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
what holds and why, which is also why `LockManager` and `mvcc` exist as
types here without being wired into anything yet (see Features).

## Key Components

- `checkpoint` - `write_checkpoint`, writes a fuzzy checkpoint. See
  [checkpoint.MD](src/checkpoint.MD).
- `error` - `TxnError`, errors raised by the transaction subsystem. See
  [error.MD](src/error.MD).
- `isolation` - `IsolationLevel`, the isolation level a transaction runs
  under. See [isolation.MD](src/isolation.MD).
- `lock_manager` - `LockManager`, `LockMode`: grants and releases row-level
  locks under two-phase locking. See
  [lock_manager.MD](src/lock_manager.MD).
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
threshold. Isolation holds only trivially, because `engine::Database`
executes one statement at a time on a single thread — there is no
concurrent transaction for one transaction's uncommitted writes to be
isolated *from* yet.

`LockManager::lock`/`unlock` and `VersionChain::visible_version` are all
`todo!()` — neither is called anywhere in the engine today. That's roadmap
milestone M10 (concurrent transactions): two-phase locking via
`LockManager` first, then MVCC via `VersionChain` for snapshot isolation,
wired into every read and write path once concurrent execution exists to
isolate. See `docs/ROADMAP.md` and `docs/adr/0004-acid-scope.md`.

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
not just that the types compile. `tests/smoke.rs` is the minimum-viable
compile-and-construct check. Run just this crate with:

```sh
cargo test -p txn
```
