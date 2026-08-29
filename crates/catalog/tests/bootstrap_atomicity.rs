use std::error::Error;
use std::fs::OpenOptions;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use catalog::Catalog;
use common::TxnId;
use storage::block_device::{BlockDevice, DurabilityModel, FaultyDevice, FileDevice};
use storage::buffer::BufferPool;
use storage::disk::DiskManager;
use storage::dwb::DoubleWriteBuffer;
use storage::page::PAGE_SIZE;
use storage::recovery;
use storage::replacer::LruKReplacer;
use storage::wal::{
    DEFAULT_SEGMENT_SIZE, FaultySegmentStore, LogManager, LogRecordKind, SegmentStore,
};

const BOOTSTRAP_TXN: TxnId = TxnId(0);

fn open_file(path: &Path) -> std::io::Result<std::fs::File> {
    OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path)
}

fn faulty_pool(
    dir: &Path,
    counter: &Arc<AtomicU64>,
    fail_at: u64,
) -> Result<BufferPool, Box<dyn Error>> {
    let db_device: Box<dyn BlockDevice> = Box::new(FaultyDevice::new(
        Box::new(FileDevice::new(open_file(&dir.join("test.db"))?)),
        counter.clone(),
        fail_at,
    ));
    let wal_store: Arc<dyn SegmentStore> = Arc::new(FaultySegmentStore::new(
        dir.join("test.db.wal"),
        counter.clone(),
        fail_at,
        DurabilityModel::write_is_durable(),
    ));
    let disk = DiskManager::open_with_device(db_device, PAGE_SIZE, None)?;
    let dwb =
        DoubleWriteBuffer::open(dir.join("test.db.dwb"), DoubleWriteBuffer::DEFAULT_CAPACITY)?;
    let log = LogManager::open_with_segment_store(wal_store, DEFAULT_SEGMENT_SIZE)?;
    Ok(BufferPool::new(disk, dwb, log, 16, Box::new(LruKReplacer::new(16, 2))))
}

fn real_pool(dir: &Path) -> Result<BufferPool, Box<dyn Error>> {
    let disk = DiskManager::open(dir.join("test.db"), PAGE_SIZE)?;
    let dwb =
        DoubleWriteBuffer::open(dir.join("test.db.dwb"), DoubleWriteBuffer::DEFAULT_CAPACITY)?;
    let log = LogManager::open(dir.join("test.db.wal"))?;
    Ok(BufferPool::new(disk, dwb, log, 16, Box::new(LruKReplacer::new(16, 2))))
}

fn run_bootstrap(pool: &BufferPool) -> Result<(), Box<dyn Error>> {
    pool.append_log(BOOTSTRAP_TXN, LogRecordKind::Begin)?;
    Catalog::open(pool, BOOTSTRAP_TXN)?;
    let commit_lsn = pool.append_log(BOOTSTRAP_TXN, LogRecordKind::Commit)?;
    pool.flush_log(commit_lsn)?;
    pool.append_log(BOOTSTRAP_TXN, LogRecordKind::End)?;
    pool.flush_log_all()?;
    pool.flush_all()?;
    pool.sync()?;
    Ok(())
}

fn total_bootstrap_write_count(dir: &Path) -> Result<u64, Box<dyn Error>> {
    let counter = Arc::new(AtomicU64::new(0));
    let pool = faulty_pool(dir, &counter, u64::MAX)?;
    run_bootstrap(&pool)?;
    Ok(counter.load(Ordering::Relaxed))
}

#[test]
fn the_bootstrap_heap_and_its_header_pointer_become_durable_together_or_not_at_all()
-> Result<(), Box<dyn Error>> {
    let count_dir = tempfile::tempdir()?;
    let k = total_bootstrap_write_count(count_dir.path())?;
    assert!(k > 0, "the bootstrap sequence must perform at least one write");

    for n in 1..=k {
        let dir = tempfile::tempdir()?;

        let counter = Arc::new(AtomicU64::new(0));
        let attempt = || -> Result<(), Box<dyn Error>> {
            let pool = faulty_pool(dir.path(), &counter, n)?;
            run_bootstrap(&pool)
        };
        let _ = attempt();

        let recovered = real_pool(dir.path())?;
        recovery::recover(&recovered)?;

        match recovered.catalog_first_page()? {
            None => {}
            Some(_) => {
                let catalog = Catalog::open(&recovered, TxnId(1))?;
                assert_eq!(
                    catalog.table_names(),
                    Vec::<&str>::new(),
                    "n={n}/{k}: a freshly recovered bootstrap heap must contain no tables yet"
                );
            }
        }
    }
    Ok(())
}
