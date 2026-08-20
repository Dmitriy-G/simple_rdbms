use std::collections::HashMap;

use common::TxnId;

use crate::error::TxnError;
use crate::isolation::IsolationLevel;
use crate::transaction::Transaction;

/// Owns the lifecycle of every transaction: assigning ids, tracking active
/// transactions, and driving commit/abort (which, once the WAL exists,
/// means writing the corresponding log record and releasing the
/// transaction's locks).
#[derive(Debug, Default)]
pub struct TransactionManager {
    active: HashMap<TxnId, Transaction>,
    #[allow(dead_code)]
    next_txn_id: u64,
}

impl TransactionManager {
    /// Creates a manager with no active transactions.
    pub fn new() -> Self {
        Self { active: HashMap::new(), next_txn_id: 0 }
    }

    /// Begins a new transaction at the given isolation level, returning its
    /// id.
    pub fn begin(&mut self, isolation_level: IsolationLevel) -> TxnId {
        let _ = isolation_level;
        todo!("allocate the next TxnId, insert a Transaction, log a Begin record")
    }

    /// Commits `txn_id`: logs a commit record, releases its locks, and
    /// removes it from the active set.
    pub fn commit(&mut self, txn_id: TxnId) -> Result<(), TxnError> {
        let _ = txn_id;
        todo!("log a Commit record, release all locks held, remove from active")
    }

    /// Aborts `txn_id`: undoes its writes via the WAL, releases its locks,
    /// and removes it from the active set.
    pub fn abort(&mut self, txn_id: TxnId) -> Result<(), TxnError> {
        let _ = txn_id;
        todo!("walk the undo chain via last_lsn, release all locks, remove from active")
    }

    /// Looks up the current state of an active transaction.
    pub fn get(&self, txn_id: TxnId) -> Result<&Transaction, TxnError> {
        self.active.get(&txn_id).ok_or(TxnError::UnknownTransaction(txn_id.0))
    }
}
