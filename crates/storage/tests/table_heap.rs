use std::collections::HashMap;
use std::error::Error;

use common::TxnId;
use proptest::prelude::*;
use proptest::test_runner::{Config, RngAlgorithm, TestCaseError, TestRng, TestRunner};
use storage::StorageError;
use storage::buffer::BufferPool;
use storage::heap::{MAX_SLOTS, MAX_TUPLE_SIZE, TableHeap};
use storage::page::PAGE_SIZE;
use test_support::PoolOptions;

const TXN: TxnId = TxnId(0);

fn open_pool(pool_size: usize) -> Result<(BufferPool, tempfile::TempDir), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let pool = test_support::open_pool(dir.path(), PoolOptions::new(pool_size))?;
    Ok((pool, dir))
}

#[test]
fn insert_spans_pages_and_iterates_every_tuple_back() -> Result<(), Box<dyn Error>> {
    let (pool, _dir) = open_pool(4)?;
    let mut heap = TableHeap::create(&pool, TXN)?;

    let payload = vec![0x42u8; 300];
    let mut expected: HashMap<common::Rid, Vec<u8>> = HashMap::new();
    for i in 0..40u32 {
        let mut tuple = payload.clone();
        tuple.extend_from_slice(&i.to_le_bytes());
        let rid = heap.insert_tuple(TXN, &tuple)?;
        expected.insert(rid, tuple);
    }

    for (rid, tuple_bytes) in &expected {
        assert_eq!(heap.get_tuple(*rid)?.as_deref(), Some(tuple_bytes.as_slice()));
    }

    let mut seen: HashMap<common::Rid, Vec<u8>> = HashMap::new();
    for entry in heap.iter() {
        let (rid, bytes) = entry?;
        seen.insert(rid, bytes);
    }
    assert_eq!(seen, expected);

    Ok(())
}

#[test]
fn insert_tuple_too_large_for_any_page_errors_cleanly() -> Result<(), Box<dyn Error>> {
    let (pool, _dir) = open_pool(4)?;
    let mut heap = TableHeap::create(&pool, TXN)?;

    let oversized = vec![0u8; MAX_TUPLE_SIZE + 1];
    match heap.insert_tuple(TXN, &oversized) {
        Err(StorageError::TupleTooLarge { .. }) => {}
        Err(other) => panic!("expected TupleTooLarge, got {other}"),
        Ok(_) => panic!("an oversized tuple must not be accepted"),
    }

    Ok(())
}

#[test]
fn get_tuple_returns_none_for_deleted_slot() -> Result<(), Box<dyn Error>> {
    let (pool, _dir) = open_pool(4)?;
    let mut heap = TableHeap::create(&pool, TXN)?;

    let rid = heap.insert_tuple(TXN, b"gone soon")?;
    heap.delete_tuple(TXN, rid)?;

    assert_eq!(heap.get_tuple(rid)?, None);
    Ok(())
}

#[test]
fn update_tuple_in_place_overwrites_a_sub_range_without_changing_length_or_rid()
-> Result<(), Box<dyn Error>> {
    let (pool, _dir) = open_pool(4)?;
    let mut heap = TableHeap::create(&pool, TXN)?;

    let before = heap.insert_tuple(TXN, b"before")?;
    let target = heap.insert_tuple(TXN, b"aaaaaaaaaa")?;
    let after = heap.insert_tuple(TXN, b"after")?;

    heap.update_tuple_in_place(TXN, target, 3, b"BBBB")?;

    assert_eq!(heap.get_tuple(target)?.as_deref(), Some(b"aaaBBBBaaa".as_slice()));
    assert_eq!(heap.get_tuple(before)?.as_deref(), Some(b"before".as_slice()));
    assert_eq!(heap.get_tuple(after)?.as_deref(), Some(b"after".as_slice()));
    Ok(())
}

#[test]
fn update_tuple_in_place_past_the_tuples_end_is_rejected() -> Result<(), Box<dyn Error>> {
    let (pool, _dir) = open_pool(4)?;
    let mut heap = TableHeap::create(&pool, TXN)?;
    let rid = heap.insert_tuple(TXN, b"short")?;

    match heap.update_tuple_in_place(TXN, rid, 3, b"toolong") {
        Err(StorageError::InPlaceUpdateOutOfBounds { .. }) => {}
        Err(other) => panic!("expected InPlaceUpdateOutOfBounds, got {other}"),
        Ok(()) => panic!("a patch running past the tuple's own length must not be accepted"),
    }
    assert_eq!(
        heap.get_tuple(rid)?.as_deref(),
        Some(b"short".as_slice()),
        "a rejected update must leave the original bytes untouched"
    );
    Ok(())
}

#[test]
fn update_tuple_in_place_on_a_deleted_slot_is_rejected() -> Result<(), Box<dyn Error>> {
    let (pool, _dir) = open_pool(4)?;
    let mut heap = TableHeap::create(&pool, TXN)?;
    let rid = heap.insert_tuple(TXN, b"gone")?;
    heap.delete_tuple(TXN, rid)?;

    match heap.update_tuple_in_place(TXN, rid, 0, b"new!") {
        Err(StorageError::CorruptPage { .. }) => {}
        Err(other) => panic!("expected CorruptPage, got {other}"),
        Ok(()) => panic!("updating a tombstoned slot must not be accepted"),
    }
    Ok(())
}

#[test]
fn scanning_a_page_whose_init_never_reached_disk_is_a_clean_empty_scan()
-> Result<(), Box<dyn Error>> {
    let (pool, _dir) = open_pool(4)?;
    let (page_id, guard) = pool.new_page(TXN)?;
    drop(guard);

    let heap = TableHeap::open(&pool, page_id);
    let tuples: Result<Vec<_>, StorageError> = heap.iter().collect();
    assert_eq!(tuples?, Vec::new(), "an uninitialized page must scan as empty, not corrupt");
    Ok(())
}

#[test]
fn inserting_into_a_never_initialized_page_still_works() -> Result<(), Box<dyn Error>> {
    let (pool, _dir) = open_pool(4)?;
    let (page_id, guard) = pool.new_page(TXN)?;
    drop(guard);

    let mut heap = TableHeap::open(&pool, page_id);
    let rid = heap.insert_tuple(TXN, b"lost its init write, still usable")?;
    assert_eq!(
        heap.get_tuple(rid)?.as_deref(),
        Some(b"lost its init write, still usable".as_slice())
    );
    Ok(())
}

#[test]
fn slot_count_above_max_slots_still_reports_corruption() -> Result<(), Box<dyn Error>> {
    let (pool, _dir) = open_pool(4)?;
    let (page_id, mut guard) = pool.new_page(TXN)?;
    guard.write(TXN, 12, &(MAX_SLOTS + 1).to_le_bytes())?;
    drop(guard);

    let heap = TableHeap::open(&pool, page_id);
    match heap.iter().next() {
        Some(Err(StorageError::CorruptPage { .. })) => {}
        Some(Ok(_)) => panic!("a bogus slot count must not yield a tuple"),
        Some(Err(err)) => panic!("expected CorruptPage, got a different error: {err}"),
        None => panic!("a bogus slot count must be reported, not read as a clean empty scan"),
    }
    Ok(())
}

#[test]
fn no_page_content_can_panic_a_reader() -> Result<(), Box<dyn Error>> {
    let mut runner = TestRunner::new_with_rng(
        Config { cases: 64, ..Config::default() },
        TestRng::from_seed(RngAlgorithm::ChaCha, &[0x5eu8; 32]),
    );

    let strategy = proptest::collection::vec(any::<u8>(), PAGE_SIZE);
    let outcome = runner.run(&strategy, |raw| {
        let (pool, _dir) = open_pool(2).map_err(|err| TestCaseError::fail(err.to_string()))?;
        let (page_id, mut guard) =
            pool.new_page(TXN).map_err(|err| TestCaseError::fail(err.to_string()))?;
        guard.write(TXN, 0, &raw).map_err(|err| TestCaseError::fail(err.to_string()))?;
        drop(guard);

        let mut heap = TableHeap::open(&pool, page_id);

        for entry in heap.iter().take(10_000) {
            let _ = entry;
        }
        for slot in 0..=u16::MAX {
            let _ = heap.get_tuple(common::Rid::new(page_id, slot));
        }
        let _ = heap.insert_tuple(TXN, b"probe");

        Ok(())
    });
    outcome.map_err(|err| format!("no_page_content_can_panic_a_reader: {err}"))?;
    Ok(())
}
