use std::error::Error;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use common::{Lsn, PageId, TxnId};
use storage::StorageError;
use storage::block_device::BlockDevice;
use storage::buffer::BufferPool;
use storage::disk::DiskManager;
use storage::dwb::DoubleWriteBuffer;
use storage::page::PAGE_SIZE;
use storage::recovery;
use storage::replacer::LruKReplacer;
use storage::wal::{FileSegmentStore, LogManager, LogRecord, LogRecordKind, SegmentStore};

mod support;
use support::CountingDevice;

const SMALL_SEGMENT: u64 = 512;

struct CountingSegmentStore {
    inner: FileSegmentStore,
    calls: Arc<AtomicUsize>,
    bytes: Arc<AtomicUsize>,
}

impl SegmentStore for CountingSegmentStore {
    fn existing_segments(&self) -> Result<Vec<u64>, StorageError> {
        self.inner.existing_segments()
    }

    fn open(&self, id: u64) -> Result<Box<dyn BlockDevice>, StorageError> {
        let device = self.inner.open(id)?;
        Ok(Box::new(CountingDevice::new(device, self.calls.clone(), self.bytes.clone())))
    }

    fn remove(&self, id: u64) -> Result<(), StorageError> {
        self.inner.remove(id)
    }
}

type Counters = (Arc<AtomicUsize>, Arc<AtomicUsize>);

fn open_counting(
    path: &Path,
    target_segment_size: u64,
) -> Result<(LogManager, Counters), Box<dyn Error>> {
    let calls = Arc::new(AtomicUsize::new(0));
    let bytes = Arc::new(AtomicUsize::new(0));
    let store = Arc::new(CountingSegmentStore {
        inner: FileSegmentStore::new(path),
        calls: calls.clone(),
        bytes: bytes.clone(),
    });
    let log = LogManager::open_with_segment_store(store, target_segment_size)?;
    Ok((log, (calls, bytes)))
}

fn open_pool(dir: &Path, target_segment_size: u64) -> Result<BufferPool, Box<dyn Error>> {
    let disk = DiskManager::open(dir.join("test.db"), PAGE_SIZE)?;
    let dwb =
        DoubleWriteBuffer::open(dir.join("test.db.dwb"), DoubleWriteBuffer::DEFAULT_CAPACITY)?;
    let log = LogManager::open_with_segment_size(dir.join("test.db.wal"), target_segment_size)?;
    Ok(BufferPool::new(disk, dwb, log, 8, Box::new(LruKReplacer::new(8, 2))))
}

fn filler(log: &LogManager, txn_id: TxnId) -> Result<Lsn, StorageError> {
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

#[test]
fn opening_a_long_log_reads_only_the_live_tail() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("test.wal");

    {
        let (log, ..) = open_counting(&path, SMALL_SEGMENT)?;
        let mut last_lsn = Lsn(0);
        for i in 0..400u64 {
            last_lsn = filler(&log, TxnId(i % 5))?;
        }
        log.truncate_below(last_lsn)?;
    }

    let (log, (_calls, bytes)) = open_counting(&path, SMALL_SEGMENT)?;
    drop(log);
    let bytes_read = bytes.load(Ordering::Relaxed) as u64;
    assert!(
        bytes_read < 2 * SMALL_SEGMENT,
        "reopen read {bytes_read} bytes, expected less than {}",
        2 * SMALL_SEGMENT
    );
    Ok(())
}

#[test]
fn a_checkpoint_deletes_segments_below_the_recovery_bound() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("test.wal");

    let (log, ..) = open_counting(&path, SMALL_SEGMENT)?;
    let mut lsns = Vec::new();
    for i in 0..400u64 {
        lsns.push(filler(&log, TxnId(i % 5))?);
    }

    let store = FileSegmentStore::new(&path);
    let segments_before = store.existing_segments()?.len();
    assert!(segments_before >= 3, "test needs at least 3 segments to be meaningful");

    let bound = lsns[lsns.len() / 2];
    log.truncate_below(bound)?;

    let segments_after = store.existing_segments()?.len();
    assert!(segments_after < segments_before, "truncation must have dropped some segments");

    let early_lsn = lsns[0];
    assert_eq!(
        log.read_at(early_lsn)?,
        None,
        "a record from a truncated segment must no longer be readable"
    );

    let late_lsn = *lsns.last().unwrap();
    assert!(
        log.read_at(late_lsn)?.is_some(),
        "a record from a retained segment must still be readable"
    );

    Ok(())
}

#[test]
fn a_transaction_open_across_a_checkpoint_can_still_be_undone() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("test.wal");

    let (log, ..) = open_counting(&path, SMALL_SEGMENT)?;
    const ACTIVE: TxnId = TxnId(999);
    let begin_lsn = log.append(LogRecord { txn_id: ACTIVE, kind: LogRecordKind::Begin })?;
    log.flush(begin_lsn)?;

    for i in 0..400u64 {
        filler(&log, TxnId(i % 5))?;
    }

    let later_lsn = log.append(LogRecord {
        txn_id: ACTIVE,
        kind: LogRecordKind::Update {
            page_id: PageId(2),
            offset: 0,
            before: vec![0; 8],
            after: vec![1; 8],
        },
    })?;
    log.flush(later_lsn)?;

    log.truncate_below(begin_lsn)?;

    let reread_begin = log.read_at(begin_lsn)?.expect("the active transaction's Begin survives");
    assert_eq!(reread_begin.kind, LogRecordKind::Begin);

    let reread_later = log.read_at(later_lsn)?.expect("its later record also survives");
    assert_eq!(reread_later.prev_lsn, Some(begin_lsn));

    Ok(())
}

#[test]
fn recovery_across_a_segment_boundary_reproduces_the_committed_prefix() -> Result<(), Box<dyn Error>>
{
    let dir = tempfile::tempdir()?;

    let page_id;
    {
        let pool = open_pool(dir.path(), SMALL_SEGMENT)?;
        let (pid, mut guard) = pool.new_page(TxnId(1))?;
        page_id = pid;
        guard.write(TxnId(1), 16, b"before-boundary")?;
        drop(guard);

        for i in 0..200u64 {
            let mut guard = pool.fetch_page(page_id)?;
            guard.write(TxnId(2 + i), 40, &(i as u32).to_le_bytes())?;
            drop(guard);
        }

        let mut guard = pool.fetch_page(page_id)?;
        guard.write(TxnId(1), 16, b"after--boundary")?;
        drop(guard);
        let commit_lsn = pool.append_log(TxnId(1), LogRecordKind::Commit)?;
        pool.flush_log(commit_lsn)?;
        assert!(
            pool.log_bytes_appended() > SMALL_SEGMENT,
            "test needs the workload to span more than one segment"
        );
    }

    let pool = open_pool(dir.path(), SMALL_SEGMENT)?;
    recovery::recover(&pool)?;

    let guard = pool.fetch_page(page_id)?;
    assert_eq!(&guard.page().data()[16..31], b"after--boundary");
    Ok(())
}
