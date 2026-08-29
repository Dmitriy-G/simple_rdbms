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
use storage::wal::{
    FileSegmentStore, HEADER_LEN, LogManager, LogRecord, LogRecordKind, SegmentStore, segment_path,
};

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

#[test]
fn a_roll_survives_an_unclean_reopen() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("test.wal");

    let lsns_before = {
        let (log, ..) = open_counting(&path, SMALL_SEGMENT)?;
        let mut lsns = Vec::new();
        for i in 0..100u64 {
            lsns.push(filler(&log, TxnId(i % 5))?);
        }
        (lsns, log.segment_ids())
    };
    let (lsns_before, segment_ids_before) = lsns_before;
    assert!(
        segment_ids_before.len() >= 2,
        "test needs at least one roll to have happened, got segments {segment_ids_before:?}"
    );

    let (log, ..) = open_counting(&path, SMALL_SEGMENT)?;
    let segment_ids_after = log.segment_ids();
    assert_eq!(
        segment_ids_after, segment_ids_before,
        "every segment a roll sealed, including the active one's header, must survive a reopen \
         with no clean shutdown in between"
    );

    let records: Vec<_> = log.iter_from(Lsn(HEADER_LEN))?.collect();
    let lsns_after: Vec<Lsn> = records.iter().map(|r| r.lsn).collect();
    assert_eq!(
        lsns_after, lsns_before,
        "every record, including ones appended into the newly rolled active segment, must \
         still decode to the same LSN after an unclean reopen - a misread or corrupted header \
         would either lose records or shift their addressing"
    );

    Ok(())
}

#[test]
fn a_headerless_active_segment_continues_the_sealed_lsn_space() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("test.wal");

    {
        let (log, ..) = open_counting(&path, SMALL_SEGMENT)?;
        for i in 0..400u64 {
            filler(&log, TxnId(i % 5))?;
        }
    }

    let store = FileSegmentStore::new(&path);
    let ids = store.existing_segments()?;
    assert!(ids.len() >= 3, "test needs at least one sealed segment behind the active one");
    let active_id = *ids.last().expect("at least one segment exists");
    let active_path = segment_path(&path, active_id);

    let original_active_start_lsn = {
        let bytes = std::fs::read(&active_path)?;
        u64::from_le_bytes(bytes[8..16].try_into()?)
    };

    let active_file = std::fs::OpenOptions::new().write(true).open(&active_path)?;
    active_file.set_len(0)?;
    drop(active_file);

    let (log, ..) = open_counting(&path, SMALL_SEGMENT)?;
    let next_lsn = filler(&log, TxnId(99))?;
    assert!(
        next_lsn.0 >= original_active_start_lsn,
        "a record appended after reopening past a headerless active segment must not reuse LSNs \
         already used by sealed segments: got {next_lsn:?}, sealed segments end at \
         {original_active_start_lsn}"
    );

    Ok(())
}

#[test]
fn a_corrupt_sealed_segment_is_refused_rather_than_silently_shortened() -> Result<(), Box<dyn Error>>
{
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("test.wal");

    {
        let (log, ..) = open_counting(&path, SMALL_SEGMENT)?;
        for i in 0..400u64 {
            filler(&log, TxnId(i % 5))?;
        }
    }

    let store = FileSegmentStore::new(&path);
    let ids = store.existing_segments()?;
    assert!(ids.len() >= 3, "test needs at least two sealed segments to be meaningful");
    let first_sealed_id = ids[0];
    let first_sealed_path = segment_path(&path, first_sealed_id);

    let original_len = std::fs::metadata(&first_sealed_path)?.len();
    let mut bytes = std::fs::read(&first_sealed_path)?;
    let mid = bytes.len() / 2;
    bytes[mid] ^= 0xFF;
    std::fs::write(&first_sealed_path, &bytes)?;

    let store: Arc<dyn SegmentStore> = Arc::new(FileSegmentStore::new(&path));
    match LogManager::open_with_segment_store(store, SMALL_SEGMENT) {
        Ok(_) => panic!("a corrupt sealed segment must be refused, not silently opened"),
        Err(err) => assert!(
            matches!(err, StorageError::CorruptLogHeader { .. }),
            "expected CorruptLogHeader, got {err:?}"
        ),
    }

    let new_len = std::fs::metadata(&first_sealed_path)?.len();
    assert_eq!(
        new_len, original_len,
        "a refused open must not have truncated the corrupt sealed segment - the pre-fix code \
         would pass a \"reopen errors\" test while having already shortened the file"
    );

    Ok(())
}
