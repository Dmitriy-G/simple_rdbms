use common::{Lsn, TxnId};
use txn::{IsolationLevel, LockManager, Transaction, TransactionManager, TransactionState};

#[test]
fn transaction_constructs_growing() {
    let txn = Transaction::new(TxnId(1), IsolationLevel::SnapshotIsolation, Lsn(8));
    assert_eq!(txn.state, TransactionState::Growing);
}

#[test]
fn transaction_manager_and_lock_manager_construct() {
    let manager = TransactionManager::new(None);
    assert!(manager.get(TxnId(1)).is_err());

    let _lock_manager = LockManager::new();
}
