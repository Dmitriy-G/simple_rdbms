use std::collections::{BTreeMap, HashSet};
use std::error::Error;
use std::path::Path;

use common::{PageId, Rid, TxnId};
use storage::StorageError;
use storage::block_device::DurabilityModel;
use storage::btree::BTreeIndex;
use storage::buffer::BufferPool;
use storage::disk::DiskManager;
use storage::dwb::DoubleWriteBuffer;
use storage::page::PAGE_SIZE;
use storage::recovery;
use storage::replacer::LruKReplacer;
use storage::wal::{DEFAULT_SEGMENT_SIZE, LogManager, LogRecordKind};
use test_support::{CrashWorkload, DeviceTriple, assert_workload_is_crash_safe};

fn open_recovered_pool(devices: DeviceTriple) -> Result<BufferPool, StorageError> {
    let (db_device, wal_store, dwb_device) = devices;
    let disk_manager = DiskManager::open_with_device(db_device, PAGE_SIZE, None)?;
    let log_manager = LogManager::open_with_segment_store(wal_store, DEFAULT_SEGMENT_SIZE)?;
    let dwb = DoubleWriteBuffer::open_with_device(dwb_device, DoubleWriteBuffer::DEFAULT_CAPACITY)?;
    recovery::recover_double_write(&disk_manager, &dwb)?;
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

fn duplicate_workload_keys() -> Vec<Vec<u8>> {
    let mut low = b"000low".to_vec();
    low.resize(600, b'a');
    let mut hot = b"hot".to_vec();
    hot.resize(600, b'h');
    let mut high = b"999high".to_vec();
    high.resize(600, b'z');

    let mut keys = vec![low];
    keys.extend((0..10).map(|_| hot.clone()));
    keys.push(high);
    keys
}

fn commit(pool: &BufferPool, txn_id: TxnId) -> Result<(), StorageError> {
    let commit_lsn = pool.append_log(txn_id, LogRecordKind::Commit)?;
    pool.flush_log(commit_lsn)?;
    pool.append_log(txn_id, LogRecordKind::End)?;
    Ok(())
}

struct BTreeWorkload<'a> {
    keys: &'a [Vec<u8>],
}

impl CrashWorkload for BTreeWorkload<'_> {
    type Handle = BufferPool;
    type State = BTreeMap<Vec<u8>, Vec<Rid>>;

    fn item_count(&self) -> usize {
        self.keys.len()
    }

    fn open(&self, _dir: &Path, devices: DeviceTriple) -> Result<Self::Handle, Box<dyn Error>> {
        Ok(open_recovered_pool(devices)?)
    }

    fn drive(&self, pool: &mut Self::Handle) -> usize {
        let pool: &BufferPool = pool;
        let create: Result<PageId, StorageError> = (|| {
            let index = BTreeIndex::create(pool, TxnId(0))?;
            pool.set_catalog_first_page(TxnId(0), index.root_page_id())?;
            commit(pool, TxnId(0))?;
            Ok(index.root_page_id())
        })();
        let Ok(mut root_page_id) = create else { return 0 };

        let mut committed = 0usize;
        for (i, key) in self.keys.iter().enumerate() {
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

    fn expected_state(&self, safe_prefix: usize) -> Result<Self::State, Box<dyn Error>> {
        let mut map: BTreeMap<Vec<u8>, Vec<Rid>> = BTreeMap::new();
        for (i, key) in self.keys.iter().enumerate() {
            let entry = map.entry(key.clone()).or_default();
            if i < safe_prefix {
                entry.push(Rid::new(PageId(1), i as u16));
            }
        }
        for rids in map.values_mut() {
            rids.sort_by_key(|rid| (rid.page_id, rid.slot));
        }
        Ok(map)
    }

    fn observed_state(
        &self,
        safe_prefix: usize,
        pool: &mut Self::Handle,
    ) -> Result<Self::State, Box<dyn Error>> {
        let root_page_id = pool.catalog_first_page()?;
        if safe_prefix > 0 && root_page_id.is_none() {
            return Err("at least one key was safely committed, so its transaction's commit \
                         made the tree's creation durable too - a durable root pointer must exist"
                .into());
        }

        let mut map: BTreeMap<Vec<u8>, Vec<Rid>> = BTreeMap::new();
        let mut seen: HashSet<&[u8]> = HashSet::new();
        for key in self.keys {
            if !seen.insert(key.as_slice()) {
                continue;
            }
            let rids = match root_page_id {
                Some(root_page_id) => {
                    let index = BTreeIndex::open(pool, root_page_id);
                    index
                        .check_invariants(None)
                        .map_err(|reason| -> Box<dyn Error> { reason.into() })?;
                    let mut rids = index.get(key)?;
                    rids.sort_by_key(|rid| (rid.page_id, rid.slot));
                    rids
                }
                None => Vec::new(),
            };
            map.insert(key.clone(), rids);
        }
        Ok(map)
    }
}

#[test]
fn workload_keys_actually_force_a_root_split() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let counter = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let devices = test_support::faulty_devices(
        dir.path(),
        &counter,
        u64::MAX,
        DurabilityModel::write_is_durable(),
    )?;
    let pool = open_recovered_pool(devices)?;
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
fn duplicate_workload_keys_actually_force_a_split_from_duplicates() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let counter = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let devices = test_support::faulty_devices(
        dir.path(),
        &counter,
        u64::MAX,
        DurabilityModel::write_is_durable(),
    )?;
    let pool = open_recovered_pool(devices)?;
    let original_root = BTreeIndex::create(&pool, TxnId(0))?.root_page_id();

    let mut index = BTreeIndex::open(&pool, original_root);
    let keys = duplicate_workload_keys();
    for (i, key) in keys.iter().enumerate() {
        index.insert(TxnId(i as u64 + 1), key, Rid::new(PageId(1), i as u16))?;
    }
    assert_ne!(
        index.root_page_id(),
        original_root,
        "the duplicate-heavy workload must be large enough to force at least one split"
    );
    index.check_invariants(None).map_err(|e| -> Box<dyn Error> { e.into() })?;

    let mut hot = b"hot".to_vec();
    hot.resize(600, b'h');
    assert_eq!(index.get(&hot)?.len(), 10, "all ten hot-key inserts must be findable");
    Ok(())
}

#[test]
fn enough_inserts_to_force_a_root_split_survive_a_crash_at_every_write()
-> Result<(), Box<dyn Error>> {
    let keys = workload_keys();
    assert_workload_is_crash_safe(
        &BTreeWorkload { keys: &keys },
        DurabilityModel::write_is_durable(),
    )
}

#[test]
fn enough_inserts_to_force_a_root_split_survive_a_crash_at_every_write_with_unsynced_writes_lost()
-> Result<(), Box<dyn Error>> {
    let keys = workload_keys();
    assert_workload_is_crash_safe(&BTreeWorkload { keys: &keys }, DurabilityModel::requires_sync())
}

#[test]
fn enough_inserts_to_force_a_root_split_survive_a_crash_at_every_write_with_torn_writes()
-> Result<(), Box<dyn Error>> {
    let keys = workload_keys();
    assert_workload_is_crash_safe(&BTreeWorkload { keys: &keys }, DurabilityModel::torn_write())
}

#[test]
fn enough_inserts_to_force_a_root_split_survive_a_crash_at_every_write_with_torn_and_unsynced_writes()
-> Result<(), Box<dyn Error>> {
    let keys = workload_keys();
    assert_workload_is_crash_safe(
        &BTreeWorkload { keys: &keys },
        DurabilityModel::torn_write_requires_sync(),
    )
}

#[test]
fn duplicate_heavy_workload_survives_a_crash_at_every_write() -> Result<(), Box<dyn Error>> {
    let keys = duplicate_workload_keys();
    assert_workload_is_crash_safe(
        &BTreeWorkload { keys: &keys },
        DurabilityModel::write_is_durable(),
    )
}

#[test]
fn duplicate_heavy_workload_survives_a_crash_at_every_write_with_unsynced_writes_lost()
-> Result<(), Box<dyn Error>> {
    let keys = duplicate_workload_keys();
    assert_workload_is_crash_safe(&BTreeWorkload { keys: &keys }, DurabilityModel::requires_sync())
}

#[test]
fn duplicate_heavy_workload_survives_a_crash_at_every_write_with_torn_writes()
-> Result<(), Box<dyn Error>> {
    let keys = duplicate_workload_keys();
    assert_workload_is_crash_safe(&BTreeWorkload { keys: &keys }, DurabilityModel::torn_write())
}

#[test]
fn duplicate_heavy_workload_survives_a_crash_at_every_write_with_torn_and_unsynced_writes()
-> Result<(), Box<dyn Error>> {
    let keys = duplicate_workload_keys();
    assert_workload_is_crash_safe(
        &BTreeWorkload { keys: &keys },
        DurabilityModel::torn_write_requires_sync(),
    )
}
