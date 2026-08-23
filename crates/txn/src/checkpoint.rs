use common::Lsn;
use storage::buffer::BufferPool;
use storage::wal::{CHECKPOINT_TXN, LogRecordKind};

use crate::error::TxnError;
use crate::isolation::IsolationLevel;
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
    txn_manager: &mut TransactionManager,
) -> Result<Lsn, TxnError> {
    let begin_lsn = pool.append_log(CHECKPOINT_TXN, LogRecordKind::CheckpointBegin)?;
    let att = txn_manager.active_snapshot(pool);
    let dpt = pool.dirty_page_table();
    pool.append_log(CHECKPOINT_TXN, LogRecordKind::CheckpointEnd { att, dpt })?;
    pool.flush_log_all()?;

    // Recording the checkpoint's own LSN in the header (page 0) is an
    // ordinary, logged page mutation like any other, so - per M11's "route
    // page 0 through the WAL" - it needs a real transaction of its own,
    // committed immediately: a header write that could outlive a crash
    // without ever being logged is exactly the bug that fixed.
    let header_txn = txn_manager.begin(pool, IsolationLevel::ReadCommitted)?;
    pool.set_last_checkpoint_lsn(header_txn, begin_lsn)?;
    txn_manager.commit(header_txn, pool)?;

    Ok(begin_lsn)
}
