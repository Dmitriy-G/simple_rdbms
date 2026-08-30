use std::collections::HashMap;

use common::crc::crc32;
use common::{Lsn, PageId, TxnId};

use crate::buffer::BufferPool;
use crate::disk::DiskManager;
use crate::dwb::DoubleWriteBuffer;
use crate::error::StorageError;
use crate::page::{self, Page};
use crate::wal::{CHECKPOINT_TXN, HEADER_LEN, LogRecordKind};

pub fn recover_double_write(
    disk: &DiskManager,
    dwb: &DoubleWriteBuffer,
) -> Result<(), StorageError> {
    let Some(entries) = dwb.read_batch()? else {
        return Ok(());
    };

    let mut restored_pages = Vec::new();
    for (index, &(page_id, slot_crc)) in entries.iter().enumerate() {
        let slot = dwb.read_slot(index)?;
        let actual_crc = crc32(&slot);
        if actual_crc != slot_crc {
            if entries.iter().any(|&(_, other_crc)| other_crc == actual_crc) {
                return Err(StorageError::DoubleWriteRestoreFailed { page_id: page_id.0 });
            }
            continue;
        }
        if slot.iter().all(|&b| b == 0) {
            continue;
        }

        let mut real_page = Page::new(page_id);
        match disk.read_page_unchecked(page_id, &mut real_page) {
            Ok(()) => {
                if !page::checksum_ok(real_page.data()) {
                    let mut restored = Page::new(page_id);
                    restored.data_mut().copy_from_slice(&slot);
                    disk.write_page(page_id, &restored)?;
                    restored_pages.push(page_id);
                }
            }
            Err(StorageError::PageNotFound(_)) => {}
            Err(err) => return Err(err),
        }
    }

    disk.sync()?;

    for page_id in &restored_pages {
        let mut check = Page::new(*page_id);
        disk.read_page_unchecked(*page_id, &mut check)?;
        if !page::checksum_ok(check.data()) {
            return Err(StorageError::DoubleWriteRestoreFailed { page_id: page_id.0 });
        }
    }

    if !restored_pages.is_empty() {
        metrics::counter!("dwb_pages_restored_total").increment(restored_pages.len() as u64);
        tracing::warn!(
            pages = restored_pages.len(),
            "double-write buffer restored torn pages during recovery - the last shutdown was \
             unclean"
        );
    }

    dwb.clear_batch()?;
    Ok(())
}

#[tracing::instrument(skip_all)]
pub fn recover(pool: &BufferPool) -> Result<Option<TxnId>, StorageError> {
    let recovery_start = std::time::Instant::now();
    let start = pool.last_checkpoint_lsn()?.unwrap_or(Lsn(HEADER_LEN));

    let mut att: HashMap<TxnId, (Lsn, bool)> = HashMap::new();
    let mut dpt: HashMap<PageId, Lsn> = HashMap::new();

    for record in pool.log_iter_from(start)? {
        if record.txn_id != CHECKPOINT_TXN {
            continue;
        }
        if let LogRecordKind::CheckpointEnd { att: snap_att, dpt: snap_dpt } = &record.kind {
            for (txn_id, lsn) in snap_att {
                att.insert(*txn_id, (*lsn, false));
            }
            for (page_id, lsn) in snap_dpt {
                dpt.insert(*page_id, *lsn);
            }
            break;
        }
    }

    let mut records_scanned: u64 = 0;
    for record in pool.log_iter_from(start)? {
        if record.txn_id == CHECKPOINT_TXN {
            continue;
        }
        records_scanned += 1;

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
            LogRecordKind::Begin => {
                att.insert(record.txn_id, (record.lsn, false));
            }
            LogRecordKind::Abort => {
                att.entry(record.txn_id)
                    .and_modify(|entry| entry.0 = record.lsn)
                    .or_insert((record.lsn, false));
            }
            LogRecordKind::CheckpointBegin | LogRecordKind::CheckpointEnd { .. } => {
                unreachable!("checkpoint records are always logged under CHECKPOINT_TXN")
            }
        }
    }

    let winners = att.values().filter(|(_, committed)| *committed).count();
    let losers: Vec<(TxnId, Lsn)> = att
        .into_iter()
        .filter_map(|(txn_id, (last_lsn, committed))| (!committed).then_some((txn_id, last_lsn)))
        .collect();
    let loser_count = losers.len();

    let mut redo_replayed: u64 = 0;
    if let Some(&min_recovery_lsn) = dpt.values().min() {
        for record in pool.log_iter_from(min_recovery_lsn)? {
            match &record.kind {
                LogRecordKind::Update { page_id, offset, after, .. }
                | LogRecordKind::Clr { page_id, offset, after, .. } => {
                    if pool.page_lsn(*page_id)? < record.lsn {
                        pool.stamp_write(*page_id, *offset as usize, after, record.lsn)?;
                        redo_replayed += 1;
                    }
                }
                LogRecordKind::AllocPage { page_id } => {
                    pool.ensure_page_allocated(*page_id)?;
                    redo_replayed += 1;
                }
                _ => {}
            }
        }
    }

    for (txn_id, last_lsn) in losers {
        undo_transaction(pool, txn_id, last_lsn)?;
    }

    pool.flush_log_all()?;
    pool.flush_all()?;
    pool.sync()?;

    let highest_txn_id = pool.max_txn_id();
    let elapsed = recovery_start.elapsed();
    metrics::histogram!("recovery_duration_seconds").record(elapsed.as_secs_f64());
    metrics::counter!("recovery_losers_total").increment(loser_count as u64);
    tracing::info!(
        winners,
        losers = loser_count,
        records_scanned,
        redo_replayed,
        duration_ms = elapsed.as_millis() as u64,
        "recovery complete"
    );
    Ok(highest_txn_id)
}

pub fn undo_transaction(pool: &BufferPool, txn_id: TxnId, from: Lsn) -> Result<(), StorageError> {
    let mut cursor = Some(from);
    while let Some(lsn) = cursor {
        let Some(record) = pool.read_log_at(lsn)? else {
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
            LogRecordKind::Begin => {
                cursor = None;
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
