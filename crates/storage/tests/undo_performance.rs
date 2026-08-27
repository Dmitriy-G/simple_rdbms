use std::error::Error;
use std::fs::OpenOptions;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use common::TxnId;
use storage::block_device::{BlockDevice, FileDevice};
use storage::buffer::BufferPool;
use storage::disk::DiskManager;
use storage::dwb::DoubleWriteBuffer;
use storage::page::PAGE_SIZE;
use storage::recovery::undo_transaction;
use storage::replacer::LruKReplacer;
use storage::wal::LogManager;

mod support;

use support::CountingDevice;

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
    let calls = Arc::new(AtomicUsize::new(0));
    let bytes = Arc::new(AtomicUsize::new(0));
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
