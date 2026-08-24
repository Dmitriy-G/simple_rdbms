use common::Lsn;
use storage::buffer::BufferPool;
use storage::wal::{CHECKPOINT_TXN, LogRecordKind};

use crate::error::TxnError;
use crate::isolation::IsolationLevel;
use crate::manager::TransactionManager;

pub fn write_checkpoint(
    pool: &BufferPool,
    txn_manager: &mut TransactionManager,
) -> Result<Lsn, TxnError> {
    let begin_lsn = pool.append_log(CHECKPOINT_TXN, LogRecordKind::CheckpointBegin)?;
    let att = txn_manager.active_snapshot(pool);
    let dpt = pool.dirty_page_table();
    pool.append_log(CHECKPOINT_TXN, LogRecordKind::CheckpointEnd { att, dpt })?;
    pool.flush_log_all()?;

    let header_txn = txn_manager.begin(pool, IsolationLevel::ReadCommitted)?;
    pool.set_last_checkpoint_lsn(header_txn, begin_lsn)?;
    txn_manager.commit(header_txn, pool)?;

    tracing::info!(lsn = begin_lsn.0, "checkpoint complete");
    Ok(begin_lsn)
}
