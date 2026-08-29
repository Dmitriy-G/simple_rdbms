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
  its writes have been (or are being) undone.
- `Transaction` - the struct. `txn_id` is the transaction's unique id,
  assigned at `begin`. `isolation_level` is the isolation level this
  transaction runs under. `state` is the transaction's current position in
  the 2PL protocol. `begin_lsn` is the LSN of this transaction's own
  `Begin` record - the earliest LSN its undo chain could ever need to
  reach back to, which is what `TransactionManager::earliest_active_begin_lsn`
  reads to keep the write-ahead log from truncating a segment an active
  transaction's undo might still need
  (`docs/adr/0011-segmented-write-ahead-log.md`).
- `Transaction::new(txn_id, isolation_level, begin_lsn) -> Self` - begins a
  new transaction with the given id, isolation level, and `Begin` LSN, in
  the `Growing` state.

## Usage Example

```rust
use common::{Lsn, TxnId};
use txn::{IsolationLevel, Transaction};

let txn = Transaction::new(TxnId(1), IsolationLevel::ReadCommitted, Lsn(16));
assert_eq!(txn.state, txn::TransactionState::Growing);
```

`TransactionManager::begin` constructs a `Transaction` and stores it in
its active map, keyed by `txn_id`; `TransactionManager::get` returns a
reference to it for callers that need to inspect its isolation level or
state.
