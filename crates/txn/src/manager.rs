use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use common::{DbConfig, Lsn, TxnId};
use storage::buffer::BufferPool;
use storage::recovery;
use storage::wal::LogRecordKind;

use crate::error::TxnError;
use crate::isolation::IsolationLevel;
use crate::lock_manager::LockManager;
use crate::transaction::{Transaction, TransactionState};
use crate::version_store::VersionStore;

#[must_use = "an abort that is never finished leaves the transaction in the active set still holding every lock it took"]
#[derive(Debug)]
pub struct PendingAbort {
    txn_id: TxnId,
    last_lsn: Option<Lsn>,
}

impl PendingAbort {
    pub fn txn_id(&self) -> TxnId {
        self.txn_id
    }

    pub fn undo(&self, pool: &BufferPool) -> Result<(), TxnError> {
        if let Some(last_lsn) = self.last_lsn {
            recovery::undo_transaction(pool, self.txn_id, last_lsn)?;
        }
        Ok(())
    }
}

#[derive(Debug, Default)]
pub struct TransactionManager {
    active: HashMap<TxnId, Transaction>,
    next_txn_id: u64,
    lock_manager: Arc<LockManager>,
    version_store: Arc<VersionStore>,
    next_ts: u64,
}

impl TransactionManager {
    pub fn new(highest_seen: Option<TxnId>) -> Self {
        Self::with_lock_wait_timeout(highest_seen, DbConfig::DEFAULT_LOCK_WAIT_TIMEOUT_MS)
    }

    pub fn with_lock_wait_timeout(highest_seen: Option<TxnId>, lock_wait_timeout_ms: u64) -> Self {
        let next_txn_id = highest_seen.map_or(0, |TxnId(id)| id + 1);
        Self {
            active: HashMap::new(),
            next_txn_id,
            lock_manager: Arc::new(LockManager::with_timeout(Duration::from_millis(
                lock_wait_timeout_ms,
            ))),
            version_store: Arc::new(VersionStore::new()),
            next_ts: 0,
        }
    }

    pub fn lock_manager(&self) -> &Arc<LockManager> {
        &self.lock_manager
    }

    pub fn version_store(&self) -> &Arc<VersionStore> {
        &self.version_store
    }

    fn next_timestamp(&mut self) -> u64 {
        let ts = self.next_ts;
        self.next_ts += 1;
        ts
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
        let read_ts = self.next_timestamp();
        let begin_lsn = pool.append_log(txn_id, LogRecordKind::Begin)?;
        self.active.insert(txn_id, Transaction::new(txn_id, isolation_level, begin_lsn, read_ts));
        Ok(txn_id)
    }

    #[tracing::instrument(skip_all, fields(txn_id = txn_id.0))]
    pub fn commit(&mut self, txn_id: TxnId, pool: &BufferPool) -> Result<(), TxnError> {
        let txn = self.active.get(&txn_id).ok_or(TxnError::UnknownTransaction(txn_id.0))?;
        if txn.state == TransactionState::Aborted {
            return Err(TxnError::AbortInProgress(txn_id.0));
        }
        let commit_lsn = pool.append_log(txn_id, LogRecordKind::Commit)?;
        pool.flush_log(commit_lsn)?;
        let commit_ts = self.next_timestamp();
        self.version_store.commit_versions(txn_id, commit_ts);
        pool.append_log(txn_id, LogRecordKind::End)?;
        self.active.remove(&txn_id);
        self.version_store.prune(self.oldest_active_read_ts());
        self.lock_manager.release_all(txn_id);
        self.lock_manager.prune_finished(self.oldest_active_txn_id());
        metrics::counter!("transactions_committed_total").increment(1);
        Ok(())
    }

    #[tracing::instrument(skip_all, fields(txn_id = txn_id.0))]
    pub fn begin_abort(
        &mut self,
        txn_id: TxnId,
        pool: &BufferPool,
    ) -> Result<PendingAbort, TxnError> {
        let txn = self.active.get_mut(&txn_id).ok_or(TxnError::UnknownTransaction(txn_id.0))?;
        if txn.state == TransactionState::Aborted {
            return Err(TxnError::AbortInProgress(txn_id.0));
        }
        txn.state = TransactionState::Aborted;
        tracing::warn!("transaction abort");
        Ok(PendingAbort { txn_id, last_lsn: pool.last_lsn(txn_id) })
    }

    #[tracing::instrument(skip_all, fields(txn_id = pending.txn_id.0))]
    pub fn finish_abort(&mut self, pending: PendingAbort) -> Result<(), TxnError> {
        let txn_id = pending.txn_id;
        self.active.remove(&txn_id).ok_or(TxnError::UnknownTransaction(txn_id.0))?;
        self.version_store.abort_versions(txn_id, self.next_ts);
        self.version_store.prune(self.oldest_active_read_ts());
        self.lock_manager.release_all(txn_id);
        self.lock_manager.prune_finished(self.oldest_active_txn_id());
        metrics::counter!("transactions_aborted_total").increment(1);
        Ok(())
    }

    pub fn cancel_abort(&mut self, pending: PendingAbort) {
        if let Some(txn) = self.active.get_mut(&pending.txn_id) {
            txn.state = TransactionState::Growing;
        }
    }

    pub fn abort(&mut self, txn_id: TxnId, pool: &BufferPool) -> Result<(), TxnError> {
        let pending = self.begin_abort(txn_id, pool)?;
        match pending.undo(pool) {
            Ok(()) => self.finish_abort(pending),
            Err(err) => {
                self.cancel_abort(pending);
                Err(err)
            }
        }
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

    pub fn oldest_active_read_ts(&self) -> u64 {
        self.active.values().map(|txn| txn.read_ts).min().unwrap_or(self.next_ts)
    }

    pub fn oldest_active_txn_id(&self) -> TxnId {
        self.active.keys().min().copied().unwrap_or(TxnId(self.next_txn_id))
    }
}
