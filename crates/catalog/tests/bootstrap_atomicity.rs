//! The catalog's own bootstrap heap allocation (`AllocPage`) and the
//! database header's `catalog_first_page` pointer to it (a page-0 `Update`)
//! are two separate WAL records that must become durable together or not at
//! all - per `task.MD`'s "route page 0 through the WAL": a header pointing
//! at a heap whose allocation never survived a crash would be a dangling
//! pointer, not just wasted space. Sweeps every possible crash point across
//! the very first ever open of a fresh database, since that's the one
//! moment this sequence runs.

use std::cell::Cell;
use std::error::Error;
use std::fs::OpenOptions;
use std::path::Path;
use std::rc::Rc;

use catalog::Catalog;
use common::TxnId;
use storage::block_device::{BlockDevice, FaultyDevice, FileDevice};
use storage::buffer::BufferPool;
use storage::disk::DiskManager;
use storage::page::PAGE_SIZE;
use storage::recovery;
use storage::replacer::LruKReplacer;
use storage::wal::{LogManager, LogRecordKind};

const BOOTSTRAP_TXN: TxnId = TxnId(0);

fn open_file(path: &Path) -> std::io::Result<std::fs::File> {
    OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path)
}

fn faulty_pool(
    dir: &Path,
    counter: &Rc<Cell<u64>>,
    fail_at: u64,
) -> Result<BufferPool, Box<dyn Error>> {
    let db_device: Box<dyn BlockDevice> = Box::new(FaultyDevice::new(
        Box::new(FileDevice::new(open_file(&dir.join("test.db"))?)),
        counter.clone(),
        fail_at,
    ));
    let wal_device: Box<dyn BlockDevice> = Box::new(FaultyDevice::new(
        Box::new(FileDevice::new(open_file(&dir.join("test.db.wal"))?)),
        counter.clone(),
        fail_at,
    ));
    let disk = DiskManager::open_with_device(db_device, PAGE_SIZE, None)?;
    let log = LogManager::open_with_device(wal_device)?;
    Ok(BufferPool::new(disk, log, 16, Box::new(LruKReplacer::new(16, 2))))
}

fn real_pool(dir: &Path) -> Result<BufferPool, Box<dyn Error>> {
    let disk = DiskManager::open(dir.join("test.db"), PAGE_SIZE)?;
    let log = LogManager::open(dir.join("test.db.wal"))?;
    Ok(BufferPool::new(disk, log, 16, Box::new(LruKReplacer::new(16, 2))))
}

/// Exactly what `Database::open_with_managers` does the very first time a
/// database is ever opened: begin a real transaction, let `Catalog::open`
/// provision its bootstrap heap (and point the header at it) if needed,
/// then commit.
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

/// Runs `run_bootstrap` to completion against a fault-free, counting device
/// pair, returning the number of writes it performs - the upper bound for
/// the crash-injection sweep below.
fn total_bootstrap_write_count(dir: &Path) -> Result<u64, Box<dyn Error>> {
    let counter = Rc::new(Cell::new(0));
    let pool = faulty_pool(dir, &counter, u64::MAX)?;
    run_bootstrap(&pool)?;
    Ok(counter.get())
}

#[test]
fn the_bootstrap_heap_and_its_header_pointer_become_durable_together_or_not_at_all()
-> Result<(), Box<dyn Error>> {
    let count_dir = tempfile::tempdir()?;
    let k = total_bootstrap_write_count(count_dir.path())?;
    assert!(k > 0, "the bootstrap sequence must perform at least one write");

    for n in 1..=k {
        let dir = tempfile::tempdir()?;

        let counter = Rc::new(Cell::new(0));
        // A crash partway through: whatever step fails - including
        // constructing the pool itself, which does its own durable writes
        // - stop right there, exactly like a real crash. An `Err` here is
        // expected and not itself a test failure.
        let attempt = || -> Result<(), Box<dyn Error>> {
            let pool = faulty_pool(dir.path(), &counter, n)?;
            run_bootstrap(&pool)
        };
        let _ = attempt();

        let recovered = real_pool(dir.path())?;
        recovery::recover(&recovered)?;

        match recovered.catalog_first_page()? {
            // Neither the heap allocation nor the header pointer survived -
            // fine, `Catalog::open` will provision a fresh one next time.
            None => {}
            // The header claims a catalog heap exists at `page_id`: it must
            // actually be one. If the allocation didn't survive along with
            // it, this errors (most likely `PageNotFound`, reading past the
            // file's real extent) instead of silently trusting a dangling
            // pointer - which is exactly what this test is sweeping every
            // crash point to rule out.
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
