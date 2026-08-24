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
  the 2PL protocol.
- `Transaction::new(txn_id, isolation_level) -> Self` - begins a new
  transaction with the given id and isolation level, in the `Growing`
  state.

## Usage Example

```rust
use common::TxnId;
use txn::{IsolationLevel, Transaction};

let txn = Transaction::new(TxnId(1), IsolationLevel::ReadCommitted);
assert_eq!(txn.state, txn::TransactionState::Growing);
```

`TransactionManager::begin` constructs a `Transaction` and stores it in
its active map, keyed by `txn_id`; `TransactionManager::get` returns a
reference to it for callers that need to inspect its isolation level or
state.
