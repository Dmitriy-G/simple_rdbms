//! `LogManager` behavior: a mix of record kinds survives an append/flush/
//! reopen round trip with `prev_lsn` chained correctly per transaction; a
//! log file truncated mid-record (a crash mid-append) iterates cleanly up
//! to the last intact record instead of erroring; a single flipped byte
//! fails CRC at exactly the record it corrupted and stops there; and the
//! write-ahead invariant itself - no page ever reaches disk before the log
//! record describing it is durable - holds across a real, eviction-heavy
//! workload.

use std::error::Error;

use common::{Lsn, PageId, TxnId};
use storage::buffer::BufferPool;
use storage::disk::DiskManager;
use storage::heap::TableHeap;
use storage::page::PAGE_SIZE;
use storage::replacer::LruKReplacer;
use storage::wal::{LogManager, LogRecord, LogRecordKind};

#[test]
fn round_trip_mixed_record_kinds_survives_reopen() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("test.wal");

    let records = vec![
        LogRecord { txn_id: TxnId(1), kind: LogRecordKind::Begin },
        LogRecord {
            txn_id: TxnId(1),
            kind: LogRecordKind::Update {
                page_id: PageId(3),
                offset: 10,
                before: vec![1, 2, 3],
                after: vec![4, 5, 6],
            },
        },
        LogRecord { txn_id: TxnId(2), kind: LogRecordKind::AllocPage { page_id: PageId(4) } },
        LogRecord { txn_id: TxnId(1), kind: LogRecordKind::Commit },
        LogRecord { txn_id: TxnId(1), kind: LogRecordKind::End },
        LogRecord { txn_id: TxnId(2), kind: LogRecordKind::Abort },
    ];

    let mut lsns = Vec::new();
    {
        let mut log = LogManager::open(path.clone())?;
        let mut last_lsn = Lsn(0);
        for record in records.clone() {
            last_lsn = log.append(record)?;
            lsns.push(last_lsn);
        }
        log.flush(last_lsn)?;
    }

    let log = LogManager::open(path.clone())?;
    let read_back: Vec<_> = log.iter_from(Lsn(0))?.collect();

    assert_eq!(read_back.len(), records.len());
    for (logged, (expected, expected_lsn)) in read_back.iter().zip(records.iter().zip(lsns.iter()))
    {
        assert_eq!(logged.lsn, *expected_lsn);
        assert_eq!(logged.txn_id, expected.txn_id);
        assert_eq!(logged.kind, expected.kind);
    }

    // `prev_lsn` chains each transaction's own records together,
    // independent of what other transactions interleaved between them.
    assert_eq!(read_back[0].prev_lsn, None, "txn 1's first record has no predecessor");
    assert_eq!(read_back[1].prev_lsn, Some(lsns[0]), "txn 1's Update chains from its Begin");
    assert_eq!(read_back[2].prev_lsn, None, "txn 2's first record has no predecessor");
    assert_eq!(read_back[3].prev_lsn, Some(lsns[1]), "txn 1's Commit chains from its Update");
    assert_eq!(read_back[4].prev_lsn, Some(lsns[3]), "txn 1's End chains from its Commit");
    assert_eq!(read_back[5].prev_lsn, Some(lsns[2]), "txn 2's Abort chains from its AllocPage");

    Ok(())
}

#[test]
fn truncated_mid_record_iterates_cleanly_up_to_last_intact_record() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("test.wal");

    let mut log = LogManager::open(path.clone())?;
    let begin_lsn = log.append(LogRecord { txn_id: TxnId(1), kind: LogRecordKind::Begin })?;
    let commit_lsn = log.append(LogRecord { txn_id: TxnId(1), kind: LogRecordKind::Commit })?;
    log.flush(commit_lsn)?;

    // Simulate a crash partway through appending a record by lopping a few
    // bytes off the end of the file - shorter than a complete record, so
    // the trailing bytes describe no valid record at all.
    let file_len = std::fs::metadata(&path)?.len();
    let file = std::fs::OpenOptions::new().write(true).open(&path)?;
    file.set_len(file_len - 3)?;
    drop(file);

    let read_back: Vec<_> = log.iter_from(Lsn(0))?.collect();
    assert_eq!(read_back.len(), 1, "the torn record must not be yielded");
    assert_eq!(read_back[0].lsn, begin_lsn);

    Ok(())
}

#[test]
fn flipped_byte_fails_crc_at_exactly_that_record_and_stops() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("test.wal");

    let mut log = LogManager::open(path.clone())?;
    let begin_lsn = log.append(LogRecord { txn_id: TxnId(1), kind: LogRecordKind::Begin })?;
    log.append(LogRecord {
        txn_id: TxnId(1),
        kind: LogRecordKind::Update {
            page_id: PageId(1),
            offset: 0,
            before: vec![0],
            after: vec![1],
        },
    })?;
    let commit_lsn = log.append(LogRecord { txn_id: TxnId(1), kind: LogRecordKind::Commit })?;
    log.flush(commit_lsn)?;

    // Flip one byte inside the second record (the `Update`), leaving the
    // first record untouched.
    let mut bytes = std::fs::read(&path)?;
    let first_record_len = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
    let flip_at = first_record_len + 5;
    bytes[flip_at] ^= 0xFF;
    std::fs::write(&path, &bytes)?;

    let read_back: Vec<_> = log.iter_from(Lsn(0))?.collect();
    assert_eq!(read_back.len(), 1, "only the untouched first record should be yielded");
    assert_eq!(read_back[0].lsn, begin_lsn);

    Ok(())
}

/// The invariant `BufferPool::flush_frame` and M7's recovery both depend
/// on: a page never reaches disk before the log record describing its most
/// recent change is durable. A three-frame pool against hundreds of
/// inserted tuples forces repeated eviction under pressure, so
/// `write_observations` captures many real disk writes to check this
/// against, not just the trivial case of a pool that never evicts.
#[test]
fn wal_ordering_holds_across_an_eviction_heavy_workload() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let disk = DiskManager::open(dir.path().join("test.db"), PAGE_SIZE)?;
    let log = LogManager::open(dir.path().join("test.db.wal"))?;
    let pool = BufferPool::new(disk, log, 3, Box::new(LruKReplacer::new(3, 2)));

    let mut heap = TableHeap::create(&pool, TxnId(0))?;
    let payload = vec![0x7Au8; 200];
    for i in 0..200u32 {
        let mut tuple = payload.clone();
        tuple.extend_from_slice(&i.to_le_bytes());
        heap.insert_tuple(TxnId(0), &tuple)?;
    }
    pool.flush_all()?;

    let observations = pool.write_observations();
    assert!(!observations.is_empty(), "the small pool should have forced at least one eviction");
    for observation in observations {
        assert!(
            observation.durable_lsn >= observation.page_lsn,
            "page {:?} reached disk with page_lsn {:?} ahead of durable_lsn {:?}",
            observation.page_id,
            observation.page_lsn,
            observation.durable_lsn
        );
    }

    Ok(())
}
