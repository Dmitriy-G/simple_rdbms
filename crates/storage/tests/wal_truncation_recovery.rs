use std::error::Error;
use std::path::Path;

use common::{Lsn, PageId, TxnId};
use storage::buffer::BufferPool;
use storage::recovery;
use storage::wal::{
    FileSegmentStore, HEADER_LEN, LogManager, LogRecord, LogRecordKind, SegmentStore, segment_path,
};
use test_support::PoolOptions;

const SMALL_SEGMENT: u64 = 512;

fn open_pool(dir: &Path, target_segment_size: u64) -> Result<BufferPool, Box<dyn Error>> {
    test_support::open_pool(dir, PoolOptions::new(8).segment_size(target_segment_size))
}

fn filler(log: &LogManager, txn_id: TxnId) -> Result<Lsn, Box<dyn Error>> {
    let lsn = log.append(LogRecord {
        txn_id,
        kind: LogRecordKind::Update {
            page_id: PageId(1),
            offset: 0,
            before: vec![0; 24],
            after: vec![1; 24],
        },
    })?;
    log.flush(lsn)?;
    Ok(lsn)
}

fn commit(pool: &BufferPool, txn_id: TxnId) -> Result<Lsn, Box<dyn Error>> {
    let commit_lsn = pool.append_log(txn_id, LogRecordKind::Commit)?;
    pool.flush_log(commit_lsn)?;
    Ok(commit_lsn)
}

fn segment_start_lsn(wal_path: &Path, id: u64) -> Result<u64, Box<dyn Error>> {
    let bytes = std::fs::read(segment_path(wal_path, id))?;
    Ok(u64::from_le_bytes(bytes[8..16].try_into()?))
}

#[test]
fn recovery_reads_sealed_segments_when_the_header_lsn_predates_them() -> Result<(), Box<dyn Error>>
{
    let dir = tempfile::tempdir()?;
    let wal_path = dir.path().join("test.wal");
    let log = LogManager::open_with_segment_size(wal_path.clone(), SMALL_SEGMENT)?;

    let mut lsns = Vec::new();
    for i in 0..400u64 {
        lsns.push(filler(&log, TxnId(i % 5))?);
    }

    let store = FileSegmentStore::new(wal_path.clone());
    let segments_before = store.existing_segments()?;
    assert!(segments_before.len() >= 3, "test needs at least 3 segments to be meaningful");

    let bound = lsns[lsns.len() / 2];
    log.truncate_below(bound)?;

    let segments_after = store.existing_segments()?;
    assert!(
        segments_after.len() >= 2,
        "test needs a sealed segment to survive truncation, not just the active one"
    );
    assert!(
        segments_after.len() < segments_before.len(),
        "truncation must have dropped some segments"
    );

    let earliest_start_lsn = segment_start_lsn(&wal_path, segments_after[0])?;

    let records: Vec<_> = log.iter_from(Lsn(HEADER_LEN))?.collect();

    let min_lsn =
        records.iter().map(|r| r.lsn.0).min().expect("some records must survive truncation");
    assert_eq!(
        min_lsn, earliest_start_lsn,
        "the iterator must start from the earliest surviving sealed segment, not skip straight \
         to the active one"
    );

    let expected_count = lsns.iter().filter(|lsn| lsn.0 >= earliest_start_lsn).count();
    assert_eq!(
        records.len(),
        expected_count,
        "every record still physically retained must be readable, not just the active segment's"
    );

    Ok(())
}

#[test]
fn a_crash_after_truncation_still_redoes_committed_work() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let db_path = dir.path().join("test.db");
    let wal_path = dir.path().join("test.db.wal");

    const ROW_OFFSET: usize = 200;

    let (page_id, mid_index, mid_value_lsn) = {
        let pool = open_pool(dir.path(), SMALL_SEGMENT)?;

        let (page_id, mut guard) = pool.new_page(TxnId(1))?;
        guard.write(TxnId(1), 16, b"first-committed-row")?;
        drop(guard);
        commit(&pool, TxnId(1))?;
        pool.flush_page(page_id)?;

        let mut commit_lsns = Vec::with_capacity(200);
        for i in 0..200u64 {
            let mut guard = pool.fetch_page(page_id)?;
            guard.write(TxnId(2 + i), ROW_OFFSET + (i as usize) * 4, &(i as u32).to_le_bytes())?;
            drop(guard);
            commit_lsns.push(commit(&pool, TxnId(2 + i))?);
            if i == 29 {
                pool.flush_page(page_id)?;
            }
        }
        assert!(
            pool.log_bytes_appended() > SMALL_SEGMENT,
            "test needs the workload to span more than one segment"
        );

        let mut guard = pool.fetch_page(page_id)?;
        guard.write(TxnId(999), 96, b"last-committed-row")?;
        drop(guard);
        commit(&pool, TxnId(999))?;

        let store = FileSegmentStore::new(wal_path.clone());
        let segments_before = store.existing_segments()?.len();
        assert!(segments_before >= 3, "test needs at least 3 segments to be meaningful");

        let dpt_min = pool
            .dirty_page_table()
            .iter()
            .map(|(_, lsn)| lsn.0)
            .min()
            .expect("the last write left the page dirty");
        pool.truncate_log_below(Lsn(dpt_min))?;

        let segments_after = store.existing_segments()?.len();
        assert!(
            (2..segments_before).contains(&segments_after),
            "truncation must drop at least one sealed segment while leaving at least one \
             sealed segment behind, got {segments_after} of {segments_before}"
        );

        let active_id =
            *store.existing_segments()?.last().expect("the active segment always exists");
        let active_start_lsn = segment_start_lsn(&wal_path, active_id)?;
        let mid_index = commit_lsns
            .iter()
            .enumerate()
            .filter(|&(i, lsn)| i > 30 && lsn.0 < active_start_lsn)
            .map(|(i, _)| i)
            .max()
            .expect(
                "the workload must leave at least one committed row in a sealed segment that \
                 survives truncation, not only the active segment",
            );
        (page_id, mid_index, commit_lsns[mid_index])
    };

    let store = FileSegmentStore::new(wal_path.clone());
    let earliest_id = store.existing_segments()?[0];
    let earliest_start_lsn = segment_start_lsn(&wal_path, earliest_id)?;
    let on_disk_header_lsn = {
        let bytes = std::fs::read(&db_path)?;
        u64::from_le_bytes(bytes[32..40].try_into()?)
    };
    let recovery_start_lsn = if on_disk_header_lsn == 0 { HEADER_LEN } else { on_disk_header_lsn };
    assert!(
        recovery_start_lsn < earliest_start_lsn,
        "test setup must leave the on-disk checkpoint LSN ({recovery_start_lsn}) behind the \
         earliest surviving segment ({earliest_start_lsn}), or this is not exercising the bug"
    );
    assert!(
        mid_value_lsn.0 >= earliest_start_lsn,
        "the asserted row's own record must still be physically retained after truncation"
    );

    let pool = open_pool(dir.path(), SMALL_SEGMENT)?;
    recovery::recover(&pool)?;

    let guard = pool.fetch_page(page_id)?;
    let data = guard.page().data();
    assert_eq!(&data[16..16 + b"first-committed-row".len()], b"first-committed-row");
    assert_eq!(&data[96..96 + b"last-committed-row".len()], b"last-committed-row");
    let mid_offset = ROW_OFFSET + mid_index * 4;
    assert_eq!(
        &data[mid_offset..mid_offset + 4],
        &(mid_index as u32).to_le_bytes(),
        "row {mid_index}, whose only surviving record lives in a sealed (not active) segment, \
         must be redone"
    );

    Ok(())
}
