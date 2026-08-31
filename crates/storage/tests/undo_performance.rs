use std::error::Error;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use common::TxnId;
use storage::buffer::BufferPool;
use storage::disk::DiskManager;
use storage::dwb::DoubleWriteBuffer;
use storage::page::PAGE_SIZE;
use storage::recovery::undo_transaction;
use storage::replacer::LruKReplacer;
use storage::wal::{DEFAULT_SEGMENT_SIZE, FileSegmentStore, LogManager, SegmentStore};
use test_support::CountingSegmentStore;

const SMALL_SEGMENT: u64 = 512;

#[test]
fn undoing_thousands_of_updates_reads_the_log_a_bounded_number_of_times()
-> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let disk = DiskManager::open(dir.path().join("test.db"), PAGE_SIZE)?;

    let calls = Arc::new(AtomicUsize::new(0));
    let bytes = Arc::new(AtomicUsize::new(0));
    let opens = Arc::new(AtomicUsize::new(0));
    let store: Arc<dyn SegmentStore> = Arc::new(CountingSegmentStore::new(
        FileSegmentStore::new(dir.path().join("test.db.wal")),
        calls.clone(),
        bytes.clone(),
        opens,
    ));
    let log = LogManager::open_with_segment_store(store, DEFAULT_SEGMENT_SIZE)?;
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

    calls.store(0, Ordering::Relaxed);
    bytes.store(0, Ordering::Relaxed);

    let last_lsn = pool.last_lsn(txn_id).ok_or("txn should have appended at least one lsn")?;
    undo_transaction(&pool, txn_id, last_lsn)?;

    assert!(
        calls.load(Ordering::Relaxed) <= 3 * N,
        "expected roughly 2 device reads per undone record, got {} calls for {N} updates",
        calls.load(Ordering::Relaxed)
    );
    assert!(
        bytes.load(Ordering::Relaxed) <= 500 * N,
        "expected a small, constant number of bytes read per undone record, got {} bytes for \
         {N} updates - this is the metric that actually catches the old quadratic behavior (N \
         calls each re-reading the whole, ever-growing durable log)",
        bytes.load(Ordering::Relaxed)
    );
    Ok(())
}

#[test]
fn undoing_across_sealed_segments_reads_the_log_a_bounded_number_of_times()
-> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let disk = DiskManager::open(dir.path().join("test.db"), PAGE_SIZE)?;

    let calls = Arc::new(AtomicUsize::new(0));
    let bytes = Arc::new(AtomicUsize::new(0));
    let opens = Arc::new(AtomicUsize::new(0));
    let store: Arc<dyn SegmentStore> = Arc::new(CountingSegmentStore::new(
        FileSegmentStore::new(dir.path().join("test.db.wal")),
        calls.clone(),
        bytes.clone(),
        opens.clone(),
    ));
    let store_for_inspection = store.clone();
    let log = LogManager::open_with_segment_store(store, SMALL_SEGMENT)?;
    let dwb = DoubleWriteBuffer::open(
        dir.path().join("test.db.dwb"),
        DoubleWriteBuffer::DEFAULT_CAPACITY,
    )?;

    let pool = BufferPool::new(disk, dwb, log, 8, Box::new(LruKReplacer::new(8, 2)));

    const N: usize = 2_000;
    let txn_id = TxnId(1);
    let (_, mut guard) = pool.new_page(txn_id)?;
    for i in 0..N {
        guard.write(txn_id, 16, &(i as u32).to_le_bytes())?;
        pool.flush_log(guard.page().page_lsn())?;
    }
    drop(guard);

    let sealed_segment_count = store_for_inspection.existing_segments()?.len().saturating_sub(1);
    assert!(
        sealed_segment_count >= 4,
        "test needs the workload to seal several segments so the undo chain actually crosses \
         them, only sealed {sealed_segment_count}"
    );

    calls.store(0, Ordering::Relaxed);
    bytes.store(0, Ordering::Relaxed);
    opens.store(0, Ordering::Relaxed);

    let last_lsn = pool.last_lsn(txn_id).ok_or("txn should have appended at least one lsn")?;
    undo_transaction(&pool, txn_id, last_lsn)?;

    let open_calls = opens.load(Ordering::Relaxed);
    assert!(
        open_calls <= sealed_segment_count + 2,
        "undoing a chain that crosses {sealed_segment_count} sealed segments opened a device \
         {open_calls} times - the undo walk moves backward through segments monotonically, so a \
         one-entry cache should open each sealed segment at most once instead of once per record"
    );
    Ok(())
}
