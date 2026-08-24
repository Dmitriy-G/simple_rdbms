use std::collections::HashMap;

use common::{Lsn, PageId, TxnId};

use crate::buffer::BufferPool;
use crate::disk::DiskManager;
use crate::dwb::DoubleWriteBuffer;
use crate::error::StorageError;
use crate::page::{self, Page};
use crate::wal::{CHECKPOINT_TXN, HEADER_LEN, LogRecordKind};

pub fn recover_double_write(
    disk: &mut DiskManager,
    dwb: &mut DoubleWriteBuffer,
) -> Result<(), StorageError> {
    let Some(page_ids) = dwb.read_batch()? else {
        return Ok(());
    };

    let mut restored_pages = Vec::new();
    for (index, page_id) in page_ids.into_iter().enumerate() {
        let slot = dwb.read_slot(index)?;
        if !page::checksum_ok(&slot) {
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

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::error::Error;
    use std::fs::OpenOptions;
    use std::rc::Rc;

    use common::TxnId;

    use super::undo_transaction;
    use crate::block_device::{BlockDevice, CountingDevice, FileDevice};
    use crate::buffer::BufferPool;
    use crate::disk::DiskManager;
    use crate::dwb::DoubleWriteBuffer;
    use crate::page::PAGE_SIZE;
    use crate::replacer::LruKReplacer;
    use crate::wal::LogManager;

    #[test]
    fn undoing_thousands_of_updates_reads_the_log_a_bounded_number_of_times()
    -> Result<(), Box<dyn Error>> {
        let dir = tempfile::tempdir()?;
        let disk = DiskManager::open(dir.path().join("test.db"), PAGE_SIZE)?;

        let wal_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.path().join("test.db.wal"))?;
        let calls = Rc::new(Cell::new(0));
        let bytes = Rc::new(Cell::new(0));
        let device: Box<dyn BlockDevice> = Box::new(CountingDevice::new(
            Box::new(FileDevice::new(wal_file)),
            calls.clone(),
            bytes.clone(),
        ));
        let log = LogManager::open_with_device(device)?;
        let dwb = DoubleWriteBuffer::open(
            dir.path().join("test.db.dwb"),
            DoubleWriteBuffer::DEFAULT_CAPACITY,
        )?;

        let pool = BufferPool::new(disk, dwb, log, 8, Box::new(LruKReplacer::new(8, 2)));

        const N: usize = 5_000;
        let txn_id = TxnId(1);
        let (_, mut guard) = pool.new_page(txn_id)?;
        for i in 0..N {
            guard.write(txn_id, 16, &(i as u32).to_le_bytes())?;
        }
        drop(guard);

        pool.flush_log_all()?;

        calls.set(0);
        bytes.set(0);

        let last_lsn = pool.last_lsn(txn_id).ok_or("txn should have appended at least one lsn")?;
        undo_transaction(&pool, txn_id, last_lsn)?;

        assert!(
            calls.get() <= 3 * N,
            "expected roughly 2 device reads per undone record, got {} calls for {N} updates",
            calls.get()
        );
        assert!(
            bytes.get() <= 500 * N,
            "expected a small, constant number of bytes read per undone record, got {} bytes \
             for {N} updates - this is the metric that actually catches the old quadratic \
             behavior (N calls each re-reading the whole, ever-growing durable log)",
            bytes.get()
        );
        Ok(())
    }
}
