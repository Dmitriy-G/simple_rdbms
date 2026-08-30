use std::error::Error;
use std::fs::OpenOptions;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use common::{DbConfig, Error as DbError};
use engine::Database;
use storage::block_device::{BlockDevice, DurabilityModel, FaultyDevice, FileDevice};
use storage::wal::{DEFAULT_SEGMENT_SIZE, FileSegmentStore, SegmentStore};

const SETUP_STATEMENT: &str = "CREATE TABLE t (a INTEGER)";

fn open_file(path: &Path) -> std::io::Result<std::fs::File> {
    OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path)
}

fn config(dir: &Path) -> DbConfig {
    DbConfig { checkpoint_byte_threshold: 1, ..DbConfig::new(dir.join("test.db")) }
}

fn open_with_counted_data_device(
    dir: &Path,
    counter: Arc<AtomicU64>,
    fail_at: u64,
) -> Result<Database, Box<dyn Error>> {
    let db_device: Box<dyn BlockDevice> = Box::new(FaultyDevice::with_model(
        Box::new(FileDevice::new(open_file(&dir.join("test.db"))?)),
        counter,
        fail_at,
        DurabilityModel::write_is_durable(),
    ));
    let wal_store: Arc<dyn SegmentStore> = Arc::new(FileSegmentStore::new(dir.join("test.db.wal")));
    let dwb_device: Box<dyn BlockDevice> =
        Box::new(FileDevice::new(open_file(&dir.join("test.db.dwb"))?));
    Ok(Database::open_with_devices(
        config(dir),
        db_device,
        wal_store,
        DEFAULT_SEGMENT_SIZE,
        dwb_device,
    )?)
}

fn total_wal_bytes(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("test.db.wal"))
        .filter_map(|entry| entry.metadata().ok())
        .map(|metadata| metadata.len())
        .sum()
}

#[test]
fn a_poisoned_pool_fails_the_next_statement_rather_than_degrading() -> Result<(), Box<dyn Error>> {
    let probe_dir = tempfile::tempdir()?;
    let probe_counter = Arc::new(AtomicU64::new(0));
    let mut probe_db =
        open_with_counted_data_device(probe_dir.path(), Arc::clone(&probe_counter), u64::MAX)?;
    probe_db.execute(SETUP_STATEMENT)?;
    let total_ticks = probe_counter.load(Ordering::Relaxed);
    assert!(total_ticks >= 2, "expected at least the header write and the checkpoint's own write");
    let fail_at = total_ticks - 1;

    let dir = tempfile::tempdir()?;
    let mut db = open_with_counted_data_device(dir.path(), Arc::new(AtomicU64::new(0)), fail_at)?;

    db.execute(SETUP_STATEMENT)?;

    let wal_bytes_after_poisoning = total_wal_bytes(dir.path());

    let result = db.execute("INSERT INTO t VALUES (1)");
    match &result {
        Ok(_) => panic!(
            "expected the poisoned pool to reject the INSERT with FlushPoisoned, but it \
             succeeded instead"
        ),
        Err(DbError::FlushPoisoned) => {}
        Err(other) => panic!("expected FlushPoisoned, got a different error instead: {other}"),
    }

    let wal_bytes_after_rejected_insert = total_wal_bytes(dir.path());
    assert_eq!(
        wal_bytes_after_rejected_insert, wal_bytes_after_poisoning,
        "a statement rejected for a poisoned pool must not append anything to the write-ahead \
         log"
    );

    Ok(())
}
