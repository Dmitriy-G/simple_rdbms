# smoke

Asserts the crate compiles and its main types can be constructed.

## Key Components

- `transaction_constructs_growing` - builds a `Transaction` via
  `Transaction::new` and checks it starts in `TransactionState::Growing`.
- `transaction_manager_and_lock_manager_construct` - builds a
  `TransactionManager` via `TransactionManager::new(None)` and checks a
  lookup on it returns an error, then builds a `LockManager`.

## Usage Example

```rust
#[test]
fn example_constructs_a_transaction_manager() {
    let manager = txn::TransactionManager::new(None);
    assert!(manager.get(common::TxnId(1)).is_err());
}
```

Run as part of `cargo test --workspace`; catches a broken build or a
signature change in `Transaction`/`TransactionManager`/`LockManager`
before it surfaces indirectly in a downstream crate's tests.
