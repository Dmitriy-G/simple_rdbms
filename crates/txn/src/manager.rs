use std::collections::HashMap;
use std::sync::Arc;

use common::{Lsn, TxnId};
use storage::buffer::BufferPool;
use storage::recovery;
use storage::wal::LogRecordKind;

use crate::error::TxnError;
use crate::isolation::IsolationLevel;
use crate::lock_manager::LockManager;
use crate::transaction::Transaction;

#[derive(Debug, Default)]
pub struct TransactionManager {
    active: HashMap<TxnId, Transaction>,
    next_txn_id: u64,
    lock_manager: Arc<LockManager>,
}

impl TransactionManager {
    pub fn new(highest_seen: Option<TxnId>) -> Self {
        let next_txn_id = highest_seen.map_or(0, |TxnId(id)| id + 1);
        Self { active: HashMap::new(), next_txn_id, lock_manager: Arc::new(LockManager::new()) }
    }

    pub fn lock_manager(&self) -> &Arc<LockManager> {
        &self.lock_manager
    }

    #[tracing::instrument(skip_all, fields(txn_id = tracing::field::Empty, isolation = ?isolation_level))]
    pub fn begin(
        &mut self,
        pool: &BufferPool,
        isolation_level: IsolationLevel,
    ) -> Result<TxnId, TxnError> {
        let txn_id = TxnId(self.next_txn_id);
        tracing::Span::current().record("txn_id", txn_id.0);
        debug_assert!(
            self.active.keys().all(|&active_id| txn_id > active_id),
            "begin assigned {txn_id:?}, which does not exceed every already-active id \
             {:?} - the id counter was not seeded past every id the log has ever used",
            self.active.keys().collect::<Vec<_>>()
        );
        self.next_txn_id += 1;
        let begin_lsn = pool.append_log(txn_id, LogRecordKind::Begin)?;
        self.active.insert(txn_id, Transaction::new(txn_id, isolation_level, begin_lsn));
        Ok(txn_id)
    }

    #[tracing::instrument(skip_all, fields(txn_id = txn_id.0))]
    pub fn commit(&mut self, txn_id: TxnId, pool: &BufferPool) -> Result<(), TxnError> {
        self.active.get(&txn_id).ok_or(TxnError::UnknownTransaction(txn_id.0))?;
        let commit_lsn = pool.append_log(txn_id, LogRecordKind::Commit)?;
        pool.flush_log(commit_lsn)?;
        pool.append_log(txn_id, LogRecordKind::End)?;
        self.active.remove(&txn_id);
        self.lock_manager.release_all(txn_id);
        metrics::counter!("transactions_committed_total").increment(1);
        Ok(())
    }

    #[tracing::instrument(skip_all, fields(txn_id = txn_id.0))]
    pub fn abort(&mut self, txn_id: TxnId, pool: &BufferPool) -> Result<(), TxnError> {
        self.active.get(&txn_id).ok_or(TxnError::UnknownTransaction(txn_id.0))?;
        tracing::warn!("transaction abort");
        if let Some(last_lsn) = pool.last_lsn(txn_id) {
            recovery::undo_transaction(pool, txn_id, last_lsn)?;
        }
        self.active.remove(&txn_id);
        self.lock_manager.release_all(txn_id);
        metrics::counter!("transactions_aborted_total").increment(1);
        Ok(())
    }

    pub fn get(&self, txn_id: TxnId) -> Result<&Transaction, TxnError> {
        self.active.get(&txn_id).ok_or(TxnError::UnknownTransaction(txn_id.0))
    }

    pub fn active_snapshot(&self, pool: &BufferPool) -> Vec<(TxnId, Lsn)> {
        self.active
            .keys()
            .filter_map(|&txn_id| pool.last_lsn(txn_id).map(|lsn| (txn_id, lsn)))
            .collect()
    }

    pub fn earliest_active_begin_lsn(&self) -> Option<Lsn> {
        self.active.values().map(|txn| txn.begin_lsn).min()
    }
}
