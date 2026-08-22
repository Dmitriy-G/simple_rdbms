//! ARIES crash recovery: Analysis, Redo, and Undo.
//!
//! `recover` is meant to be called once, on every `Database::open`, before
//! the catalog is loaded - the log and the pages it describes must be back
//! in a consistent state before anything above `storage` starts trusting
//! page contents. It never touches the catalog, transaction-manager, or
//! lock-manager concepts (this crate isn't allowed to depend on `txn`
//! anyway - see the workspace's crate dependency-edge rules), so it is
//! phrased entirely in terms of `BufferPool`/`LogManager`/`DiskManager`.
//!
//! `undo_transaction` is deliberately `pub`: it is the one piece of this
//! module a *runtime* abort (not just crash recovery) also needs, so
//! `txn::TransactionManager::abort` calls it directly rather than
//! reimplementing undo.

use std::collections::HashMap;

use common::{Lsn, PageId, TxnId};

use crate::buffer::BufferPool;
use crate::error::StorageError;
use crate::wal::{CHECKPOINT_TXN, LogRecordKind};

/// Runs Analysis, Redo, and Undo against `pool`'s write-ahead log, leaving
/// every page consistent with the log's most recent durable state and every
/// transaction that never committed fully undone. Idempotent: safe to call
/// on a log that describes a database already in a consistent state (the
/// common case - most opens follow a clean shutdown), and safe to call
/// again on a log left behind by a crash *during* a previous recovery
/// attempt (see `undo_transaction`'s docs for why).
pub fn recover(pool: &BufferPool) -> Result<(), StorageError> {
    let start = pool.last_checkpoint_lsn().unwrap_or(Lsn(1));

    // ---- Analysis ----
    // `att` maps a transaction to its most recent LSN and whether it has
    // committed (a winner) or not (still a candidate loser until the scan
    // ends). `dpt` maps a page to the LSN of the record that first dirtied
    // it since its last flush - the earliest point Redo might need to
    // start replaying from.
    let mut att: HashMap<TxnId, (Lsn, bool)> = HashMap::new();
    let mut dpt: HashMap<PageId, Lsn> = HashMap::new();

    for record in pool.log_iter_from(start)? {
        if record.txn_id == CHECKPOINT_TXN {
            if let LogRecordKind::CheckpointEnd { att: snap_att, dpt: snap_dpt } = &record.kind {
                // Only seed entries not already discovered by the scan so
                // far: anything already touched by a record after `start`
                // is strictly more current than the checkpoint's own
                // (possibly slightly stale, "fuzzy") snapshot of it.
                for (txn_id, lsn) in snap_att {
                    att.entry(*txn_id).or_insert((*lsn, false));
                }
                for (page_id, lsn) in snap_dpt {
                    dpt.entry(*page_id).or_insert(*lsn);
                }
            }
            continue;
        }

        match &record.kind {
            LogRecordKind::End => {
                att.remove(&record.txn_id);
            }
            LogRecordKind::Commit => {
                att.insert(record.txn_id, (record.lsn, true));
            }
            LogRecordKind::Update { page_id, .. }
            | LogRecordKind::Clr { page_id, .. }
            | LogRecordKind::AllocPage { page_id } => {
                att.entry(record.txn_id)
                    .and_modify(|entry| entry.0 = record.lsn)
                    .or_insert((record.lsn, false));
                dpt.entry(*page_id).or_insert(record.lsn);
            }
            LogRecordKind::Begin | LogRecordKind::Abort => {
                att.entry(record.txn_id)
                    .and_modify(|entry| entry.0 = record.lsn)
                    .or_insert((record.lsn, false));
            }
            LogRecordKind::CheckpointBegin | LogRecordKind::CheckpointEnd { .. } => {
                unreachable!("checkpoint records are always logged under CHECKPOINT_TXN")
            }
        }
    }

    let losers: Vec<(TxnId, Lsn)> = att
        .into_iter()
        .filter_map(|(txn_id, (last_lsn, committed))| (!committed).then_some((txn_id, last_lsn)))
        .collect();

    // ---- Redo ----
    // Repeats history from the oldest change that might still be missing
    // from disk, replaying every logged `Update`/`Clr`/`AllocPage` -
    // including losers' own actions, which is exactly why Undo runs after,
    // not instead of, this pass.
    if let Some(&min_recovery_lsn) = dpt.values().min() {
        for record in pool.log_iter_from(min_recovery_lsn)? {
            match &record.kind {
                LogRecordKind::Update { page_id, offset, after, .. }
                | LogRecordKind::Clr { page_id, offset, after, .. } => {
                    if pool.page_lsn(*page_id)? < record.lsn {
                        pool.stamp_write(*page_id, *offset as usize, after, record.lsn)?;
                    }
                }
                LogRecordKind::AllocPage { page_id } => {
                    pool.ensure_page_allocated(*page_id)?;
                }
                _ => {}
            }
        }
    }

    // ---- Undo ----
    for (txn_id, last_lsn) in losers {
        undo_transaction(pool, txn_id, last_lsn)?;
    }

    // Recovery leaves a durable, checkpoint-able database behind rather
    // than relying on later no-force flushes to eventually catch up.
    pool.flush_log_all()?;
    pool.flush_all()?;
    pool.sync()?;
    Ok(())
}

/// Undoes `txn_id`'s writes, walking its log chain backward starting from
/// `from` (that transaction's most recent LSN). For each `Update` found,
/// logs a `Clr` carrying the before-image and applies it, then continues
/// from the `Update`'s own `prev_lsn`; a `Clr` is never itself undone -
/// walking through one just continues from its `undo_next_lsn`. Once the
/// chain is exhausted, logs `Abort` then `End`.
///
/// This is shared by two callers: crash recovery's Undo pass (`recover`,
/// above) and `txn::TransactionManager::abort`'s ordinary runtime abort -
/// there is exactly one undo implementation, not two.
///
/// Resuming after a crash *during* undo works for free: on a second
/// recovery attempt, Analysis finds the loser still active with `last_lsn`
/// now pointing at the last `Clr` that made it to disk, and walking from
/// there immediately follows that `Clr`'s `undo_next_lsn` rather than
/// re-undoing the `Update` it already compensated for. Redo (which always
/// runs first) independently reapplies any `Clr` whose own page write
/// didn't survive the second crash, which is why Redo must replay losers'
/// actions - including their `Clr`s - and must be idempotent.
pub fn undo_transaction(pool: &BufferPool, txn_id: TxnId, from: Lsn) -> Result<(), StorageError> {
    let mut cursor = Some(from);
    while let Some(lsn) = cursor {
        let Some(record) = pool.log_iter_from(lsn)?.next() else {
            break;
        };
        match record.kind {
            LogRecordKind::Update { page_id, offset, before, .. } => {
                let undo_next_lsn = record.prev_lsn.unwrap_or(Lsn(0));
                let clr_lsn = pool.append_log(
                    txn_id,
                    LogRecordKind::Clr { page_id, offset, after: before.clone(), undo_next_lsn },
                )?;
                pool.stamp_write(page_id, offset as usize, &before, clr_lsn)?;
                cursor = record.prev_lsn;
            }
            LogRecordKind::Clr { undo_next_lsn, .. } => {
                cursor = (undo_next_lsn.0 != 0).then_some(undo_next_lsn);
            }
            _ => {
                cursor = record.prev_lsn;
            }
        }
    }
    pool.append_log(txn_id, LogRecordKind::Abort)?;
    pool.append_log(txn_id, LogRecordKind::End)?;
    Ok(())
}
