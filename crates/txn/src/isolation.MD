# isolation

The isolation level a transaction runs under, controlling which
concurrency anomalies (dirty reads, non-repeatable reads, phantoms) it is
allowed to observe.

## Key Components

- `IsolationLevel` - the enum. `ReadUncommitted` may see uncommitted
  writes from other transactions; not enforced by locking on reads.
  `ReadCommitted` never sees uncommitted writes, but a value re-read later
  in the same transaction may have changed underneath it. `RepeatableRead`
  means once a row is read, re-reading it within the same transaction
  yields the same value, but new rows matching a repeated predicate scan
  may appear (phantoms). `SnapshotIsolation` sees a single consistent
  snapshot of the database as of its start, achieved via MVCC rather than
  locking reads. `Serializable` means transactions behave as if executed
  one at a time in some serial order.

## Usage Example

```rust
use txn::IsolationLevel;

let level = IsolationLevel::ReadCommitted;
```

`TransactionManager::begin` takes an `IsolationLevel` and stores it on the
resulting `Transaction`; `engine::Database` currently always begins at
`IsolationLevel::ReadCommitted` for every statement.
