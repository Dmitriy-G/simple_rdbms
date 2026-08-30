use std::error::Error;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;

use common::TxnId;
use storage::block_device::{BlockDevice, FileDevice};
use storage::buffer::BufferPool;
use storage::disk::DiskManager;
use storage::dwb::DoubleWriteBuffer;
use storage::page::PAGE_SIZE;
use storage::replacer::LruKReplacer;
use storage::wal::LogManager;

const HEADER_COUNT_OFFSET: usize = 8;

struct BatchNestingDevice {
    inner: Box<dyn BlockDevice>,
    outstanding: Arc<AtomicBool>,
    violations: Arc<AtomicUsize>,
    batches_opened: Arc<AtomicUsize>,
}

impl BlockDevice for BatchNestingDevice {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        self.inner.read_at(offset, buf)
    }

    fn write_at(&self, offset: u64, buf: &[u8]) -> io::Result<()> {
        if offset == 0 && buf.len() >= HEADER_COUNT_OFFSET + 4 {
            let count = u32::from_le_bytes([
                buf[HEADER_COUNT_OFFSET],
                buf[HEADER_COUNT_OFFSET + 1],
                buf[HEADER_COUNT_OFFSET + 2],
                buf[HEADER_COUNT_OFFSET + 3],
            ]);
            if count > 0 {
                if self.outstanding.swap(true, Ordering::SeqCst) {
                    self.violations.fetch_add(1, Ordering::SeqCst);
                }
                self.batches_opened.fetch_add(1, Ordering::SeqCst);
            } else if !self.outstanding.swap(false, Ordering::SeqCst) {
                self.violations.fetch_add(1, Ordering::SeqCst);
            }
        }
        self.inner.write_at(offset, buf)
    }

    fn set_len(&self, len: u64) -> io::Result<()> {
        self.inner.set_len(len)
    }

    fn sync_all(&self) -> io::Result<()> {
        self.inner.sync_all()
    }

    fn size(&self) -> io::Result<u64> {
        self.inner.size()
    }
}

const RMW_PAGE_COUNT: usize = 3;
const RMW_THREAD_COUNT: usize = 8;
const RMW_POOL_SIZE: usize = 4;
const RMW_ROUNDS: usize = 100;

#[test]
fn concurrent_flushes_never_interleave_a_double_write_batch() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let db_path = dir.path().join("test.db");
    let dwb_path = dir.path().join("test.db.dwb");
    let wal_path = dir.path().join("test.db.wal");

    let disk = DiskManager::open(&db_path, PAGE_SIZE)?;
    let dwb_file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&dwb_path)?;
    let outstanding = Arc::new(AtomicBool::new(false));
    let violations = Arc::new(AtomicUsize::new(0));
    let batches_opened = Arc::new(AtomicUsize::new(0));
    let device = BatchNestingDevice {
        inner: Box::new(FileDevice::new(dwb_file)),
        outstanding: Arc::clone(&outstanding),
        violations: Arc::clone(&violations),
        batches_opened: Arc::clone(&batches_opened),
    };
    let dwb =
        DoubleWriteBuffer::open_with_device(Box::new(device), DoubleWriteBuffer::DEFAULT_CAPACITY)?;
    let log = LogManager::open(&wal_path)?;
    let replacer = Box::new(LruKReplacer::new(RMW_POOL_SIZE, 2));
    let pool = BufferPool::new(disk, dwb, log, RMW_POOL_SIZE, replacer);

    let mut page_ids = Vec::with_capacity(RMW_PAGE_COUNT);
    for _ in 0..RMW_PAGE_COUNT {
        let (page_id, guard) = pool.new_page(TxnId(0))?;
        drop(guard);
        page_ids.push(page_id);
    }
    pool.flush_all()?;

    for round in 0..RMW_ROUNDS {
        for _ in 0..RMW_POOL_SIZE {
            let (_, guard) = pool.new_page(TxnId(0))?;
            drop(guard);
        }

        let barrier = Barrier::new(RMW_THREAD_COUNT);
        thread::scope(|scope| -> Result<(), String> {
            let handles: Vec<_> = (0..RMW_THREAD_COUNT)
                .map(|thread_index| {
                    let pool = &pool;
                    let page_ids = &page_ids;
                    let barrier = &barrier;
                    scope.spawn(move || -> Result<(), String> {
                        let txn_id = TxnId((round * RMW_THREAD_COUNT + thread_index) as u64 + 1);
                        let page_id = page_ids[(thread_index + round) % page_ids.len()];
                        barrier.wait();
                        let mut guard = pool.fetch_page(page_id).map_err(|err| err.to_string())?;
                        guard
                            .write(txn_id, 200, &[thread_index as u8])
                            .map_err(|err| err.to_string())?;
                        Ok(())
                    })
                })
                .collect();
            for handle in handles {
                handle.join().expect("worker thread panicked")?;
            }
            Ok(())
        })?;
    }

    pool.flush_all()?;

    assert_eq!(
        violations.load(Ordering::SeqCst),
        0,
        "the double-write buffer's device observed an interleaved batch: one flush's \
         write_batch/clear_batch sequence overlapped another's"
    );
    assert!(
        batches_opened.load(Ordering::SeqCst) > 0,
        "no double-write batch was ever written - the test would pass vacuously without \
         eviction pressure actually driving a flush"
    );
    Ok(())
}
