use common::Lsn;
use storage::buffer::BufferPool;
use storage::wal::{CHECKPOINT_TXN, LogRecordKind};

use crate::error::TxnError;
use crate::manager::TransactionManager;

/// Writes a fuzzy checkpoint: a `CheckpointBegin` record, the current
/// Active Transaction Table and Dirty Page Table snapshots, and a
/// `CheckpointEnd` record carrying both. Records the `CheckpointBegin`
/// LSN in the database header so a future recovery knows where to start
/// scanning. Deliberately does *not* flush data pages or block any other
/// activity - a fuzzy checkpoint is exactly "log a snapshot," not a quiesce
/// point.
///
/// Lives in `txn` rather than `storage` because it is the lowest crate
/// allowed to see both an Active Transaction Table (owned by
/// `TransactionManager`) and a Dirty Page Table (owned by `BufferPool`) -
/// `storage` cannot depend on `txn` per the workspace's crate dependency
/// rules.
pub fn write_checkpoint(
    pool: &BufferPool,
    txn_manager: &TransactionManager,
) -> Result<Lsn, TxnError> {
    let begin_lsn = pool.append_log(CHECKPOINT_TXN, LogRecordKind::CheckpointBegin)?;
    let att = txn_manager.active_snapshot(pool);
    let dpt = pool.dirty_page_table();
    pool.append_log(CHECKPOINT_TXN, LogRecordKind::CheckpointEnd { att, dpt })?;
    pool.flush_log_all()?;
    pool.set_last_checkpoint_lsn(begin_lsn)?;
    Ok(begin_lsn)
}
