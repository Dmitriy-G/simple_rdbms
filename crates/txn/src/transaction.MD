# transaction

A single unit of work: the state the `TransactionManager` and
`LockManager` track for one in-flight (or finished) transaction.

## Key Components

- `TransactionState` - a flag recording whether this transaction's undo is
  running. `Growing` is the normal state, held from `Transaction::new`
  through commit and through an ordinary, uninterrupted lifetime.
  `Aborted` means all of its writes have been (or are being) undone: it is
  the one state anything currently writes, set by
  `TransactionManager::begin_abort` the moment an abort starts and cleared
  back to `Growing` by `cancel_abort` if the undo fails. So a transaction
  found in the active map marked `Aborted` is one whose undo is running
  right now with the manager's lock released (`manager.MD`,
  `docs/adr/0018-an-abort-is-not-a-quiesce-point.md`), and neither
  `commit` nor a second `begin_abort` may touch it. Strict two-phase
  locking itself is enforced elsewhere: `LockManager::release_all` inserts
  a finishing transaction's id into its `finished` set
  (`crates/txn/src/lock_manager.rs:117`), and `LockManager::acquire`
  rejects any later lock request from that id with
  `TxnError::LockAfterUnlock` (`crates/txn/src/lock_manager.rs:140-142`) -
  this field plays no part in that enforcement.
- `Transaction` - the struct. `txn_id` is the transaction's unique id,
  assigned at `begin`. `isolation_level` is the isolation level this
  transaction runs under. `state` reads `Aborted` while this transaction's
  undo is running and `Growing` otherwise. `begin_lsn` is the LSN of this transaction's own
  `Begin` record - the earliest LSN its undo chain could ever need to
  reach back to, which is what `TransactionManager::earliest_active_begin_lsn`
  reads to keep the write-ahead log from truncating a segment an active
  transaction's undo might still need
  (`docs/adr/0011-segmented-write-ahead-log.md`). `read_ts` is this
  transaction's MVCC snapshot timestamp: the value of
  `TransactionManager`'s shared timestamp counter at the moment it began,
  so it sees exactly the versions committed before it started and nothing
  committed after - a snapshot-isolation scan
  (`txn::VersionStore::is_visible_to`, which also takes the reader's own
  `txn_id` so a writer sees its own uncommitted rows) is driven by this
  field, not by `begin_lsn`, which is a WAL position rather than an MVCC
  timestamp.
- `Transaction::new(txn_id, isolation_level, begin_lsn, read_ts) -> Self` -
  begins a new transaction with the given id, isolation level, `Begin`
  LSN, and snapshot timestamp, in the `Growing` state.

## Usage Example

```rust
use common::{Lsn, TxnId};
use txn::{IsolationLevel, Transaction};

let txn = Transaction::new(TxnId(1), IsolationLevel::ReadCommitted, Lsn(16), 3);
assert_eq!(txn.state, txn::TransactionState::Growing);
```

`TransactionManager::begin` constructs a `Transaction` and stores it in
its active map, keyed by `txn_id`; `TransactionManager::get` returns a
reference to it for callers that need to inspect its isolation level or
state.
