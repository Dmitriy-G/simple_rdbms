# error

Errors raised by the transaction subsystem.

## Key Components

- `TxnError` - the enum. `DeadlockVictim(u64)` is raised when
  `LockManager::lock`/`lock_table` finds that granting the request would
  close a cycle in the wait-for graph; the detector broke the cycle by
  choosing this transaction (named by its raw id) as the victim before it
  ever blocks. `LockAfterUnlock(u64)` is raised when a transaction calls
  `LockManager::lock`/`lock_table` after `LockManager::release_all` was
  already called for it - under strict two-phase locking a transaction's
  shrinking phase is instantaneous (everything releases at once, at
  commit or abort), so this only fires on a caller bug, never as a normal
  part of the protocol. `UnknownTransaction(u64)` is raised when an
  operation referenced a transaction id the manager has no record of
  (never began, or already finished and was reaped). `Storage`
  transparently wraps a lower-level `storage::StorageError` (WAL
  append/flush, disk I/O) surfaced while logging a transaction's
  lifecycle or undoing its writes.
- `impl From<TxnError> for common::Error` - `DeadlockVictim` becomes
  `common::Error::DeadlockDetected` (SQLSTATE `40P01`, distinct from the
  `SERIALIZATION_FAILURE` `40001` a future MVCC snapshot conflict would
  raise, even though both are class 40 and equally retryable);
  `LockAfterUnlock`/`UnknownTransaction` become `common::Error::Internal`,
  since both mean this crate's own bookkeeping was violated rather than
  anything a client's SQL caused; `Storage` delegates to
  `storage::StorageError`'s own conversion. Every arm keeps the original
  variant's `Display` text as the structured error's `detail` field.

## Usage Example

```rust
use txn::TxnError;

match txn_manager.commit(txn_id, &pool) {
    Err(TxnError::UnknownTransaction(id)) => eprintln!("no such transaction: {id}"),
    _ => {}
}
```

Every fallible method on `TransactionManager` and `LockManager` returns
`Result<_, TxnError>`; callers in `engine` propagate it with `?` and rely
on the `From` impl to fold it, structured field by field, into
`common::Error` at the point it crosses into `Database::execute`.
