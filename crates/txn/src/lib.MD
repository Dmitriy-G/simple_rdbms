# lib

The transaction subsystem: transaction lifecycle, two-phase locking, and
the multi-version concurrency control types used for snapshot isolation.

`txn` depends on `storage` (a lock is taken on a physical `Rid`/`PageId`,
and MVCC version chains live alongside the tuples they version) but not on
`catalog` or `sql`: it has no notion of tables or SQL, only of
transactions and the physical records they touch.

## Key Components

- `write_checkpoint` - writes a fuzzy checkpoint. See `checkpoint.MD`.
- `TxnError` - errors raised by the transaction subsystem. See `error.MD`.
- `IsolationLevel` - the isolation level a transaction runs under. See
  `isolation.MD`.
- `LockManager`, `LockMode` - grants and releases row-level locks under
  two-phase locking. See `lock_manager.MD`.
- `TransactionManager` - owns the lifecycle of every transaction. See
  `manager.MD`.
- `VersionChain`, `VersionEntry` - the MVCC version chain for a single
  logical row. See `mvcc.MD`.
- `Transaction`, `TransactionState` - a single unit of work and its
  position in the 2PL protocol. See `transaction.MD`.

## Usage Example

```rust
use txn::{IsolationLevel, TransactionManager, write_checkpoint};

let mut txn_manager = TransactionManager::new(highest_txn_id_seen);
let txn_id = txn_manager.begin(&buffer_pool, IsolationLevel::ReadCommitted)?;
txn_manager.commit(txn_id, &buffer_pool)?;
write_checkpoint(&buffer_pool, &mut txn_manager)?;
```

`engine::Database` owns one `TransactionManager` for the lifetime of an
open database, seeded from the highest `TxnId` `storage::recovery::recover`
observed at startup; it calls `begin`/`commit`/`abort` around each
statement's execution and `write_checkpoint` once the WAL has grown past
`DbConfig::checkpoint_byte_threshold`.
