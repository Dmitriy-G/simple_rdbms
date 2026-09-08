# transaction

A single unit of work: the state the `TransactionManager` and
`LockManager` track for one in-flight (or finished) transaction.

## Key Components

- `TransactionState` - a transaction's position in the two-phase locking
  protocol: it may only acquire new locks while `Growing`, and once it
  releases any lock (moving to `Shrinking`) it may never acquire another.
  `Growing` is acquiring locks; may not yet have released any.
  `Shrinking` has released at least one lock; may not acquire any more.
  `Committed` means all of its writes are durable. `Aborted` means all of
  its writes have been (or are being) undone: it is the one state anything
  currently writes, set by `TransactionManager::begin_abort` the moment an
  abort starts and cleared back to `Growing` by `cancel_abort` if the undo
  fails. So a transaction found in the active map marked `Aborted` is one
  whose undo is running right now with the manager's lock released
  (`manager.MD`, `docs/adr/0018-an-abort-is-not-a-quiesce-point.md`), and
  neither `commit` nor a second `begin_abort` may touch it.
- `Transaction` - the struct. `txn_id` is the transaction's unique id,
  assigned at `begin`. `isolation_level` is the isolation level this
  transaction runs under. `state` is the transaction's current position in
  the 2PL protocol. `begin_lsn` is the LSN of this transaction's own
  `Begin` record - the earliest LSN its undo chain could ever need to
  reach back to, which is what `TransactionManager::earliest_active_begin_lsn`
  reads to keep the write-ahead log from truncating a segment an active
  transaction's undo might still need
  (`docs/adr/0011-segmented-write-ahead-log.md`). `read_ts` is this
  transaction's MVCC snapshot timestamp: the value of
  `TransactionManager`'s shared timestamp counter at the moment it began,
  so it sees exactly the versions committed before it started and nothing
  committed after - a snapshot-isolation scan
  (`txn::VersionStore::is_visible`) is driven by this field, not by
  `begin_lsn`, which is a WAL position rather than an MVCC timestamp.
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
