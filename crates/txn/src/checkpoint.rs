use common::{Lsn, PageId};
use storage::buffer::BufferPool;
use storage::wal::{CHECKPOINT_TXN, LogRecordKind};

use crate::error::TxnError;
use crate::isolation::IsolationLevel;
use crate::manager::TransactionManager;

#[must_use = "a checkpoint that is never finished leaves the header unflushed and the log untruncated"]
pub struct PendingCheckpoint {
    begin_lsn: Lsn,
    truncate_bound: Option<u64>,
    started: std::time::Instant,
}

impl PendingCheckpoint {
    pub fn begin_lsn(&self) -> Lsn {
        self.begin_lsn
    }
}

pub fn write_checkpoint_record(
    pool: &BufferPool,
    txn_manager: &mut TransactionManager,
) -> Result<PendingCheckpoint, TxnError> {
    let started = std::time::Instant::now();
    let begin_lsn = pool.append_log(CHECKPOINT_TXN, LogRecordKind::CheckpointBegin)?;
    let att = txn_manager.active_snapshot(pool);
    let dpt = pool.dirty_page_table();
    let dpt_min = dpt.iter().map(|(_, lsn)| lsn.0).min();
    pool.append_log(CHECKPOINT_TXN, LogRecordKind::CheckpointEnd { att, dpt })?;
    pool.flush_log_all()?;

    let header_txn = txn_manager.begin(pool, IsolationLevel::ReadCommitted)?;
    pool.set_last_checkpoint_lsn(header_txn, begin_lsn)?;
    txn_manager.commit(header_txn, pool)?;

    let att_min = txn_manager.earliest_active_begin_lsn().map(|lsn| lsn.0);
    let truncate_bound = [dpt_min, att_min, Some(begin_lsn.0)].into_iter().flatten().min();
    Ok(PendingCheckpoint { begin_lsn, truncate_bound, started })
}

pub fn finish_checkpoint(pool: &BufferPool, pending: PendingCheckpoint) -> Result<Lsn, TxnError> {
    let PendingCheckpoint { begin_lsn, truncate_bound, started } = pending;
    pool.flush_page(PageId(0))?;
    pool.sync()?;

    if let Some(truncate_bound) = truncate_bound {
        pool.truncate_log_below(Lsn(truncate_bound))?;
    }

    metrics::histogram!("checkpoint_duration_seconds").record(started.elapsed().as_secs_f64());
    metrics::gauge!("checkpoint_last_lsn").set(begin_lsn.0 as f64);
    tracing::info!(lsn = begin_lsn.0, "checkpoint complete");
    Ok(begin_lsn)
}

pub fn write_checkpoint(
    pool: &BufferPool,
    txn_manager: &mut TransactionManager,
) -> Result<Lsn, TxnError> {
    let pending = write_checkpoint_record(pool, txn_manager)?;
    finish_checkpoint(pool, pending)
}
