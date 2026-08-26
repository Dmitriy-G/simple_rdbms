use std::error::Error;
use std::path::Path;

use common::{PageId, Rid, TxnId};
use proptest::test_runner::{RngAlgorithm, TestRng};
use rand::seq::SliceRandom;
use storage::StorageError;
use storage::btree::{BTreeIndex, MAX_KEY_SIZE};
use storage::buffer::BufferPool;
use storage::disk::DiskManager;
use storage::dwb::DoubleWriteBuffer;
use storage::page::PAGE_SIZE;
use storage::replacer::LruKReplacer;
use storage::wal::LogManager;

const TXN: TxnId = TxnId(0);

fn open_pool(dir: &Path, pool_size: usize) -> Result<BufferPool, Box<dyn Error>> {
    let disk = DiskManager::open(dir.join("test.db"), PAGE_SIZE)?;
    let dwb =
        DoubleWriteBuffer::open(dir.join("test.db.dwb"), DoubleWriteBuffer::DEFAULT_CAPACITY)?;
    let log = LogManager::open(dir.join("test.db.wal"))?;
    Ok(BufferPool::new(disk, dwb, log, pool_size, Box::new(LruKReplacer::new(pool_size, 2))))
}

fn key_of(i: i32) -> Vec<u8> {
    ((i as u32) ^ 0x8000_0000).to_be_bytes().to_vec()
}

#[test]
fn a_single_insert_is_found_by_get() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let pool = open_pool(dir.path(), 16)?;
    let mut index = BTreeIndex::create(&pool, TXN)?;

    let rid = Rid::new(PageId(7), 3);
    index.insert(TXN, b"hello", rid)?;

    assert_eq!(index.get(b"hello")?, vec![rid]);
    assert_eq!(index.get(b"missing")?, Vec::new());
    index.check_invariants(None).map_err(|e| e.into())
}

#[test]
fn ascending_insert_of_ten_thousand_keys_keeps_invariants_and_every_key_findable()
-> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let pool = open_pool(dir.path(), 64)?;
    let mut index = BTreeIndex::create(&pool, TXN)?;

    for i in 0..10_000i32 {
        index.insert(TXN, &key_of(i), Rid::new(PageId(1), (i % u16::MAX as i32) as u16))?;
    }
    index.check_invariants(None).map_err(|e| -> Box<dyn Error> { e.into() })?;

    for i in 0..10_000i32 {
        let rids = index.get(&key_of(i))?;
        assert_eq!(rids.len(), 1, "key {i} should be found exactly once");
    }
    Ok(())
}

#[test]
fn descending_insert_of_ten_thousand_keys_keeps_invariants_and_every_key_findable()
-> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let pool = open_pool(dir.path(), 64)?;
    let mut index = BTreeIndex::create(&pool, TXN)?;

    for i in (0..10_000i32).rev() {
        index.insert(TXN, &key_of(i), Rid::new(PageId(1), (i % u16::MAX as i32) as u16))?;
    }
    index.check_invariants(None).map_err(|e| -> Box<dyn Error> { e.into() })?;

    for i in 0..10_000i32 {
        let rids = index.get(&key_of(i))?;
        assert_eq!(rids.len(), 1, "key {i} should be found exactly once");
    }
    Ok(())
}

#[test]
fn variable_length_varchar_keys_still_split_correctly() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let pool = open_pool(dir.path(), 64)?;
    let mut index = BTreeIndex::create(&pool, TXN)?;

    let mut keys = Vec::new();
    for i in 0..500i32 {
        let len = 1 + (i as usize * 7) % 500;
        let key = format!("{i:05}-{}", "x".repeat(len));
        keys.push(key.into_bytes());
    }
    for (i, key) in keys.iter().enumerate() {
        index.insert(TXN, key, Rid::new(PageId(1), i as u16))?;
    }
    index.check_invariants(None).map_err(|e| -> Box<dyn Error> { e.into() })?;

    for (i, key) in keys.iter().enumerate() {
        let rids = index.get(key)?;
        assert_eq!(rids, vec![Rid::new(PageId(1), i as u16)]);
    }
    Ok(())
}

#[test]
fn a_key_too_large_for_an_empty_node_errors_cleanly() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let pool = open_pool(dir.path(), 16)?;
    let mut index = BTreeIndex::create(&pool, TXN)?;

    let oversized = vec![0u8; MAX_KEY_SIZE + 1];
    match index.insert(TXN, &oversized, Rid::new(PageId(1), 0)) {
        Err(StorageError::KeyTooLarge { .. }) => {}
        Err(other) => panic!("expected KeyTooLarge, got {other}"),
        Ok(()) => panic!("an oversized key must not be accepted"),
    }
    Ok(())
}

#[test]
fn a_leaf_split_logs_well_under_the_naive_two_full_page_cost() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let pool = open_pool(dir.path(), 64)?;
    let mut index = BTreeIndex::create(&pool, TXN)?;

    let mut last_root = index.root_page_id();
    let mut found = false;
    for i in 0..3_000i32 {
        let before = pool.log_bytes_appended();
        index.insert(TXN, &key_of(i), Rid::new(PageId(1), 0))?;
        let after = pool.log_bytes_appended();
        let root_changed = index.root_page_id() != last_root;
        last_root = index.root_page_id();

        if root_changed {
            continue;
        }
        let delta = after - before;
        if delta > 500 {
            let naive_two_full_pages = 2 * (2 * PAGE_SIZE as u64);
            assert!(
                delta < naive_two_full_pages / 2,
                "a leaf split rewrites two nodes (the original leaf, mostly unchanged, plus a \
                 freshly allocated sibling); logging only the differing byte runs instead of \
                 both nodes' full before/after images every time should log well under the \
                 {naive_two_full_pages}-byte naive cost, got {delta} bytes"
            );
            found = true;
            break;
        }
    }
    assert!(found, "the insert loop must trigger at least one plain (non-root) leaf split");
    Ok(())
}

#[test]
fn root_height_grows_and_root_page_id_changes_on_a_root_split() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let pool = open_pool(dir.path(), 64)?;
    let mut index = BTreeIndex::create(&pool, TXN)?;

    let original_root = index.root_page_id();
    let mut root_changed = false;
    for i in 0..5_000i32 {
        index.insert(TXN, &key_of(i), Rid::new(PageId(1), 0))?;
        if index.root_page_id() != original_root {
            root_changed = true;
        }
    }
    assert!(root_changed, "enough inserts must eventually split the root");
    index.check_invariants(None).map_err(|e| e.into())
}

#[test]
fn duplicate_keys_spanning_a_split_are_all_returned_by_get() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let pool = open_pool(dir.path(), 64)?;
    let mut index = BTreeIndex::create(&pool, TXN)?;

    for i in 0..3_000i32 {
        index.insert(TXN, &key_of(i), Rid::new(PageId(1), 0))?;
    }
    let dup_key = key_of(1_500);
    let mut expected = vec![Rid::new(PageId(1), 0)];
    for i in 0..40u16 {
        let rid = Rid::new(PageId(2), i);
        index.insert(TXN, &dup_key, rid)?;
        expected.push(rid);
    }
    index.check_invariants(None).map_err(|e| -> Box<dyn Error> { e.into() })?;

    let mut found = index.get(&dup_key)?;
    found.sort_by_key(|rid| (rid.page_id, rid.slot));
    expected.sort_by_key(|rid| (rid.page_id, rid.slot));
    assert_eq!(found, expected);
    Ok(())
}

#[test]
fn seeded_random_permutation_of_two_thousand_keys_keeps_invariants_and_finds_every_key()
-> Result<(), Box<dyn Error>> {
    let mut rng = TestRng::from_seed(RngAlgorithm::ChaCha, &[0x5eu8; 32]);
    let mut keys: Vec<i32> = (0..2_000i32).collect();
    keys.shuffle(&mut rng);

    let dir = tempfile::tempdir()?;
    let pool = open_pool(dir.path(), 64)?;
    let mut index = BTreeIndex::create(&pool, TXN)?;

    for (i, &k) in keys.iter().enumerate() {
        index.insert(TXN, &key_of(k), Rid::new(PageId(1), (k % i32::from(u16::MAX)) as u16))?;
        if (i + 1) % 100 == 0 {
            index.check_invariants(None).map_err(|e| -> Box<dyn Error> { e.into() })?;
        }
    }
    index.check_invariants(None).map_err(|e| -> Box<dyn Error> { e.into() })?;

    for &k in &keys {
        let rids = index.get(&key_of(k))?;
        assert_eq!(rids.len(), 1, "key {k} should be found exactly once");
    }
    Ok(())
}

#[test]
fn range_scan_yields_keys_in_order_across_leaf_boundaries() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let pool = open_pool(dir.path(), 64)?;
    let mut index = BTreeIndex::create(&pool, TXN)?;

    for i in 0..3_000i32 {
        index.insert(TXN, &key_of(i), Rid::new(PageId(1), 0))?;
    }

    let collected: Result<Vec<(Vec<u8>, Rid)>, StorageError> =
        index.range_scan(None, None).collect();
    let collected = collected?;
    assert_eq!(collected.len(), 3_000);
    for w in collected.windows(2) {
        assert!(w[0].0 < w[1].0, "range_scan must yield keys in ascending order");
    }

    let bounded: Result<Vec<(Vec<u8>, Rid)>, StorageError> =
        index.range_scan(Some(&key_of(100)), Some(&key_of(110))).collect();
    let bounded = bounded?;
    assert_eq!(bounded.len(), 10, "range [100, 110) should yield exactly 10 keys");
    Ok(())
}
