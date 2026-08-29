use std::error::Error;

use common::{Lsn, PageId, TxnId};
use storage::buffer::BufferPool;
use storage::disk::DiskManager;
use storage::dwb::DoubleWriteBuffer;
use storage::heap::TableHeap;
use storage::page::PAGE_SIZE;
use storage::replacer::LruKReplacer;
use storage::wal::{HEADER_LEN, LogManager, LogRecord, LogRecordKind, segment_path};

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
        let log = LogManager::open(path.clone())?;
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

    let log = LogManager::open(path.clone())?;
    let begin_lsn = log.append(LogRecord { txn_id: TxnId(1), kind: LogRecordKind::Begin })?;
    let commit_lsn = log.append(LogRecord { txn_id: TxnId(1), kind: LogRecordKind::Commit })?;
    log.flush(commit_lsn)?;

    let segment_file = segment_path(&path, 0);
    let file_len = std::fs::metadata(&segment_file)?.len();
    let file = std::fs::OpenOptions::new().write(true).open(&segment_file)?;
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

    let log = LogManager::open(path.clone())?;
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

    let segment_file = segment_path(&path, 0);
    let mut bytes = std::fs::read(&segment_file)?;
    let header_len = HEADER_LEN as usize;
    let first_record_len = u32::from_le_bytes([
        bytes[header_len],
        bytes[header_len + 1],
        bytes[header_len + 2],
        bytes[header_len + 3],
    ]) as usize;
    let flip_at = header_len + first_record_len + 5;
    bytes[flip_at] ^= 0xFF;
    std::fs::write(&segment_file, &bytes)?;

    let read_back: Vec<_> = log.iter_from(Lsn(0))?.collect();
    assert_eq!(read_back.len(), 1, "only the untouched first record should be yielded");
    assert_eq!(read_back[0].lsn, begin_lsn);

    Ok(())
}

#[test]
fn read_at_returns_each_records_own_lsn_before_and_after_the_durable_boundary()
-> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("test.wal");
    let log = LogManager::open(path)?;

    let begin_lsn = log.append(LogRecord { txn_id: TxnId(1), kind: LogRecordKind::Begin })?;
    let commit_lsn = log.append(LogRecord { txn_id: TxnId(1), kind: LogRecordKind::Commit })?;
    log.flush(commit_lsn)?;

    let end_lsn = log.append(LogRecord { txn_id: TxnId(1), kind: LogRecordKind::End })?;

    let durable_record = log.read_at(begin_lsn)?.ok_or("begin_lsn should be durable")?;
    let boundary_record = log.read_at(commit_lsn)?.ok_or("commit_lsn should be durable")?;
    let buffered_record = log.read_at(end_lsn)?.ok_or("end_lsn should still be buffered")?;

    assert_eq!(durable_record.lsn, begin_lsn);
    assert_eq!(durable_record.kind, LogRecordKind::Begin);
    assert_eq!(boundary_record.lsn, commit_lsn);
    assert_eq!(boundary_record.kind, LogRecordKind::Commit);
    assert_eq!(buffered_record.lsn, end_lsn);
    assert_eq!(buffered_record.kind, LogRecordKind::End);

    Ok(())
}

#[test]
fn offset_lsns_survive_reopen() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("test.wal");

    {
        let log = LogManager::open(path.clone())?;
        let lsn = log.append(LogRecord { txn_id: TxnId(1), kind: LogRecordKind::Begin })?;
        log.flush(lsn)?;
    }

    let file_len = std::fs::metadata(segment_path(&path, 0))?.len();

    let log = LogManager::open(path)?;
    let next_lsn = log.append(LogRecord { txn_id: TxnId(1), kind: LogRecordKind::Commit })?;
    assert_eq!(
        next_lsn.0, file_len,
        "the next assigned LSN must equal the active segment's file length on reopen"
    );

    Ok(())
}

#[test]
fn wal_ordering_holds_across_an_eviction_heavy_workload() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let disk = DiskManager::open(dir.path().join("test.db"), PAGE_SIZE)?;
    let dwb = DoubleWriteBuffer::open(
        dir.path().join("test.db.dwb"),
        DoubleWriteBuffer::DEFAULT_CAPACITY,
    )?;
    let log = LogManager::open(dir.path().join("test.db.wal"))?;
    let pool = BufferPool::new(disk, dwb, log, 3, Box::new(LruKReplacer::new(3, 2)));

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

#[test]
fn opening_a_legacy_single_file_log_is_a_clear_error() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("test.wal");
    std::fs::write(&path, b"not a segment family, just a plain pre-segmentation log file")?;

    match LogManager::open(path) {
        Ok(_) => panic!("a bare file at the log's base path must be refused"),
        Err(err) => assert!(
            matches!(err, storage::StorageError::CorruptLogHeader { .. }),
            "expected CorruptLogHeader, got {err:?}"
        ),
    }

    Ok(())
}
