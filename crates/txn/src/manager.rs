use std::collections::HashMap;

use common::{Lsn, TxnId};
use storage::buffer::BufferPool;
use storage::recovery;
use storage::wal::LogRecordKind;

use crate::error::TxnError;
use crate::isolation::IsolationLevel;
use crate::transaction::Transaction;

/// Owns the lifecycle of every transaction: assigning ids, tracking active
/// transactions, and driving commit/abort - which means writing the
/// corresponding log record(s) and, for abort, undoing whatever the
/// transaction had already written.
#[derive(Debug, Default)]
pub struct TransactionManager {
    active: HashMap<TxnId, Transaction>,
    next_txn_id: u64,
}

impl TransactionManager {
    /// Creates a manager with no active transactions.
    pub fn new() -> Self {
        Self { active: HashMap::new(), next_txn_id: 0 }
    }

    /// Begins a new transaction at the given isolation level: assigns it
    /// the next id, logs a `Begin` record, and tracks it as active.
    /// Returns its id.
    pub fn begin(
        &mut self,
        pool: &BufferPool,
        isolation_level: IsolationLevel,
    ) -> Result<TxnId, TxnError> {
        let txn_id = TxnId(self.next_txn_id);
        self.next_txn_id += 1;
        pool.append_log(txn_id, LogRecordKind::Begin)?;
        self.active.insert(txn_id, Transaction::new(txn_id, isolation_level));
        Ok(txn_id)
    }

    /// Commits `txn_id`: logs a `Commit` record and force-flushes the log
    /// up to it (so an acknowledged commit can never be lost to a crash),
    /// logs `End`, and removes it from the active set.
    pub fn commit(&mut self, txn_id: TxnId, pool: &BufferPool) -> Result<(), TxnError> {
        self.active.get(&txn_id).ok_or(TxnError::UnknownTransaction(txn_id.0))?;
        let commit_lsn = pool.append_log(txn_id, LogRecordKind::Commit)?;
        pool.flush_log(commit_lsn)?;
        pool.append_log(txn_id, LogRecordKind::End)?;
        self.active.remove(&txn_id);
        Ok(())
    }

    /// Aborts `txn_id`: undoes every write it made (via
    /// `storage::recovery::undo_transaction`, the same routine crash
    /// recovery's own Undo pass uses), then removes it from the active set.
    /// The undo chain starts from `pool.last_lsn(txn_id)` - the log's own
    /// record of this transaction's most recent append - rather than any
    /// value cached at `begin` time, since writes made through
    /// `PageGuard::write` append directly to the log without going back
    /// through this manager.
    pub fn abort(&mut self, txn_id: TxnId, pool: &BufferPool) -> Result<(), TxnError> {
        self.active.get(&txn_id).ok_or(TxnError::UnknownTransaction(txn_id.0))?;
        if let Some(last_lsn) = pool.last_lsn(txn_id) {
            recovery::undo_transaction(pool, txn_id, last_lsn)?;
        }
        self.active.remove(&txn_id);
        Ok(())
    }

    /// Looks up the current state of an active transaction.
    pub fn get(&self, txn_id: TxnId) -> Result<&Transaction, TxnError> {
        self.active.get(&txn_id).ok_or(TxnError::UnknownTransaction(txn_id.0))
    }

    /// Every currently active transaction's id and most recent LSN, for a
    /// checkpoint's Active Transaction Table snapshot.
    pub fn active_snapshot(&self, pool: &BufferPool) -> Vec<(TxnId, Lsn)> {
        self.active
            .keys()
            .filter_map(|&txn_id| pool.last_lsn(txn_id).map(|lsn| (txn_id, lsn)))
            .collect()
    }
}
