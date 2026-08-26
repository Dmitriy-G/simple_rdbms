use std::cell::Cell;
use std::error::Error;
use std::fs::OpenOptions;
use std::path::Path;
use std::rc::Rc;

use common::{PageId, Rid, TxnId};
use storage::StorageError;
use storage::block_device::{BlockDevice, DurabilityModel, FaultyDevice, FileDevice};
use storage::btree::BTreeIndex;
use storage::buffer::BufferPool;
use storage::disk::DiskManager;
use storage::dwb::DoubleWriteBuffer;
use storage::page::PAGE_SIZE;
use storage::recovery;
use storage::replacer::LruKReplacer;
use storage::wal::{LogManager, LogRecordKind};

type DeviceTriple = (Box<dyn BlockDevice>, Box<dyn BlockDevice>, Box<dyn BlockDevice>);

fn open_file(path: &Path) -> std::io::Result<std::fs::File> {
    OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path)
}

fn faulty_devices(
    dir: &Path,
    counter: &Rc<Cell<u64>>,
    fail_at: u64,
    model: DurabilityModel,
) -> Result<DeviceTriple, Box<dyn Error>> {
    let db_file = open_file(&dir.join("test.db"))?;
    let wal_file = open_file(&dir.join("test.db.wal"))?;
    let dwb_file = open_file(&dir.join("test.db.dwb"))?;
    let db_device: Box<dyn BlockDevice> = Box::new(FaultyDevice::with_model(
        Box::new(FileDevice::new(db_file)),
        counter.clone(),
        fail_at,
        model,
    ));
    let wal_device: Box<dyn BlockDevice> = Box::new(FaultyDevice::with_model(
        Box::new(FileDevice::new(wal_file)),
        counter.clone(),
        fail_at,
        model,
    ));
    let dwb_device: Box<dyn BlockDevice> = Box::new(FaultyDevice::with_model(
        Box::new(FileDevice::new(dwb_file)),
        counter.clone(),
        fail_at,
        model,
    ));
    Ok((db_device, wal_device, dwb_device))
}

fn open_recovered_pool(
    db_device: Box<dyn BlockDevice>,
    wal_device: Box<dyn BlockDevice>,
    dwb_device: Box<dyn BlockDevice>,
) -> Result<BufferPool, StorageError> {
    let mut disk_manager = DiskManager::open_with_device(db_device, PAGE_SIZE, None)?;
    let log_manager = LogManager::open_with_device(wal_device)?;
    let mut dwb =
        DoubleWriteBuffer::open_with_device(dwb_device, DoubleWriteBuffer::DEFAULT_CAPACITY)?;
    recovery::recover_double_write(&mut disk_manager, &mut dwb)?;
    let pool =
        BufferPool::new(disk_manager, dwb, log_manager, 16, Box::new(LruKReplacer::new(16, 2)));
    recovery::recover(&pool)?;
    Ok(pool)
}

fn workload_keys() -> Vec<Vec<u8>> {
    (0..12i32)
        .map(|i| {
            let mut key = format!("{i:06}").into_bytes();
            key.resize(600, b'x');
            key
        })
        .collect()
}

fn commit(pool: &BufferPool, txn_id: TxnId) -> Result<(), StorageError> {
    let commit_lsn = pool.append_log(txn_id, LogRecordKind::Commit)?;
    pool.flush_log(commit_lsn)?;
    pool.append_log(txn_id, LogRecordKind::End)?;
    Ok(())
}

fn run_until_crash(
    db_device: Box<dyn BlockDevice>,
    wal_device: Box<dyn BlockDevice>,
    dwb_device: Box<dyn BlockDevice>,
    keys: &[Vec<u8>],
) -> usize {
    let Ok(pool) = open_recovered_pool(db_device, wal_device, dwb_device) else {
        return 0;
    };
    let pool = &pool;

    let create: Result<PageId, StorageError> = (|| {
        let index = BTreeIndex::create(pool, TxnId(0))?;
        pool.set_catalog_first_page(TxnId(0), index.root_page_id())?;
        commit(pool, TxnId(0))?;
        Ok(index.root_page_id())
    })();
    let Ok(mut root_page_id) = create else {
        return 0;
    };

    let mut committed = 0usize;
    for (i, key) in keys.iter().enumerate() {
        let txn_id = TxnId(i as u64 + 1);
        let result: Result<PageId, StorageError> = (|| {
            let mut index = BTreeIndex::open(pool, root_page_id);
            index.insert(txn_id, key, Rid::new(PageId(1), i as u16))?;
            pool.set_catalog_first_page(txn_id, index.root_page_id())?;
            commit(pool, txn_id)?;
            Ok(index.root_page_id())
        })();
        match result {
            Ok(new_root) => {
                root_page_id = new_root;
                committed += 1;
            }
            Err(_) => break,
        }
    }
    committed
}

fn assert_workload_is_crash_safe(model: DurabilityModel) -> Result<(), Box<dyn Error>> {
    let keys = workload_keys();

    let total_writes = {
        let dir = tempfile::tempdir()?;
        let counter = Rc::new(Cell::new(0));
        let (db, wal, dwb) = faulty_devices(dir.path(), &counter, u64::MAX, model)?;
        let committed = run_until_crash(db, wal, dwb, &keys);
        assert_eq!(committed, keys.len(), "an unfaulted run must commit every key");
        counter.get()
    };
    assert!(total_writes > 0, "workload must perform at least one write");

    for fail_at in 1..=total_writes {
        let dir = tempfile::tempdir()?;
        let db_path_dir = dir.path();

        let safe_prefix = {
            let counter = Rc::new(Cell::new(0));
            let (db, wal, dwb) = faulty_devices(db_path_dir, &counter, fail_at, model)?;
            run_until_crash(db, wal, dwb, &keys)
        };

        for recovery_fail_at in [1u64, 2] {
            let counter = Rc::new(Cell::new(0));
            if let Ok((db, wal, dwb)) =
                faulty_devices(db_path_dir, &counter, recovery_fail_at, model)
            {
                let _ = open_recovered_pool(db, wal, dwb);
            }
        }

        let counter = Rc::new(Cell::new(0));
        let (db, wal, dwb) = faulty_devices(db_path_dir, &counter, u64::MAX, model)?;
        let recovered = open_recovered_pool(db, wal, dwb)?;

        let root_page_id = recovered.catalog_first_page()?;
        assert!(
            safe_prefix == 0 || root_page_id.is_some(),
            "model={model:?}, fail_at={fail_at}: at least one key was safely committed, so its \
             transaction's commit made the tree's creation durable too - a durable root pointer \
             must exist"
        );

        let Some(root_page_id) = root_page_id else {
            continue;
        };
        let index = BTreeIndex::open(&recovered, root_page_id);
        index.check_invariants(None).map_err(|reason| -> Box<dyn Error> {
            format!(
                "model={model:?}, fail_at={fail_at}, safe_prefix={safe_prefix}/{}: {reason}",
                keys.len()
            )
            .into()
        })?;

        for (i, key) in keys.iter().enumerate() {
            let rids = index.get(key)?;
            if i < safe_prefix {
                assert_eq!(
                    rids,
                    vec![Rid::new(PageId(1), i as u16)],
                    "model={model:?}, fail_at={fail_at}, safe_prefix={safe_prefix}/{}: key {i} \
                     is in the committed prefix and must survive recovery",
                    keys.len()
                );
            } else {
                assert_eq!(
                    rids,
                    Vec::new(),
                    "model={model:?}, fail_at={fail_at}, safe_prefix={safe_prefix}/{}: key {i} \
                     was never committed and must not appear after recovery",
                    keys.len()
                );
            }
        }
    }
    Ok(())
}

#[test]
fn workload_keys_actually_force_a_root_split() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let counter = Rc::new(Cell::new(0));
    let (db, wal, dwb) =
        faulty_devices(dir.path(), &counter, u64::MAX, DurabilityModel::write_is_durable())?;
    let pool = open_recovered_pool(db, wal, dwb)?;
    let original_root = BTreeIndex::create(&pool, TxnId(0))?.root_page_id();

    let mut index = BTreeIndex::open(&pool, original_root);
    for (i, key) in workload_keys().iter().enumerate() {
        index.insert(TxnId(i as u64 + 1), key, Rid::new(PageId(1), i as u16))?;
    }
    assert_ne!(
        index.root_page_id(),
        original_root,
        "the crash-injection workload must be large enough to force at least one root split"
    );
    index.check_invariants(None).map_err(|e| e.into())
}

#[test]
fn enough_inserts_to_force_a_root_split_survive_a_crash_at_every_write()
-> Result<(), Box<dyn Error>> {
    assert_workload_is_crash_safe(DurabilityModel::write_is_durable())
}

#[test]
fn enough_inserts_to_force_a_root_split_survive_a_crash_at_every_write_with_unsynced_writes_lost()
-> Result<(), Box<dyn Error>> {
    assert_workload_is_crash_safe(DurabilityModel::requires_sync())
}

#[test]
fn enough_inserts_to_force_a_root_split_survive_a_crash_at_every_write_with_torn_writes()
-> Result<(), Box<dyn Error>> {
    assert_workload_is_crash_safe(DurabilityModel::torn_write())
}

#[test]
fn enough_inserts_to_force_a_root_split_survive_a_crash_at_every_write_with_torn_and_unsynced_writes()
-> Result<(), Box<dyn Error>> {
    assert_workload_is_crash_safe(DurabilityModel::torn_write_requires_sync())
}
