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
use crate::wal::{CHECKPOINT_TXN, HEADER_LEN, LogRecordKind};

/// Runs Analysis, Redo, and Undo against `pool`'s write-ahead log, leaving
/// every page consistent with the log's most recent durable state and every
/// transaction that never committed fully undone. Idempotent: safe to call
/// on a log that describes a database already in a consistent state (the
/// common case - most opens follow a clean shutdown), and safe to call
/// again on a log left behind by a crash *during* a previous recovery
/// attempt (see `undo_transaction`'s docs for why).
///
/// Returns the highest `TxnId` observed anywhere in the log (excluding
/// `CHECKPOINT_TXN`), or `None` if the log has never recorded a real
/// transaction. `Database::open` threads this into `TransactionManager::new`
/// so a freshly seeded id counter can never hand out an id the log has
/// already used - which matters even for an id whose transaction is long
/// since committed and ended, since a reused id's new `Begin` would chain
/// its `prev_lsn` right onto that unrelated transaction's own log records.
pub fn recover(pool: &BufferPool) -> Result<Option<TxnId>, StorageError> {
    let start = pool.last_checkpoint_lsn().unwrap_or(Lsn(HEADER_LEN));

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
            LogRecordKind::Begin => {
                // A `Begin` means a transaction is starting: any prior
                // entry for this id belongs to a different transaction (an
                // earlier generation reusing the id), and must not leak its
                // `committed` flag into this one - in particular, a
                // "phantom winner" left behind by a crash between a
                // `Commit` and its `End` must not make a brand new,
                // never-committed transaction of the same id look
                // committed too.
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
    Ok(pool.max_txn_id())
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
                // A transaction's undo chain never extends past its own
                // `Begin`, regardless of what `prev_lsn` this particular
                // record carries. `LogManager` chains `prev_lsn` purely by
                // `TxnId`, so if this id has ever been reused, `Begin`'s own
                // `prev_lsn` points at an unrelated, earlier generation's
                // last record for the same id - walking past it here would
                // cascade into undoing that generation's already-committed
                // writes too.
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
    use crate::page::PAGE_SIZE;
    use crate::replacer::LruKReplacer;
    use crate::wal::LogManager;

    /// Rolling back thousands of updates must cost a small, constant amount
    /// of device I/O *per undone record*, not a whole extra log-sized read
    /// for every single one of them - which is exactly the quadratic
    /// behavior `LogManager::read_at` replaced `iter_from(lsn).next()` to
    /// fix (see `task.MD`'s "Make the LSN a byte offset" task). A read
    /// counter is the deterministic way to check this; a wall-clock timing
    /// assertion would be flaky.
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

        let pool = BufferPool::new(disk, log, 8, Box::new(LruKReplacer::new(8, 2)));

        const N: usize = 5_000;
        let txn_id = TxnId(1);
        let (_, mut guard) = pool.new_page(txn_id)?;
        for i in 0..N {
            guard.write(txn_id, 16, &(i as u32).to_le_bytes())?;
        }
        drop(guard);

        // Force every one of the N updates durable, so undoing them below
        // exercises `read_at`'s device-reading branch throughout rather
        // than its free in-memory-buffer one - the branch the quadratic bug
        // actually lived in.
        pool.flush_log_all()?;

        calls.set(0);
        bytes.set(0);

        let last_lsn = pool.last_lsn(txn_id).ok_or("txn should have appended at least one lsn")?;
        undo_transaction(&pool, txn_id, last_lsn)?;

        // Two device reads per undone record (peek the length prefix, then
        // read exactly that many bytes) is what the fixed `read_at` costs;
        // this bound is generous enough to not be brittle to the exact
        // record encoding while still being far below what re-reading the
        // whole (thousands-of-records) log on every single step would cost.
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
