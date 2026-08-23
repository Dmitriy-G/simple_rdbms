//! `TableHeap` behavior: inserting enough tuples to span several pages and
//! reading them all back, and a tuple too large for any page producing a
//! clear error instead of a panic (or an infinite loop).

use std::collections::HashMap;
use std::error::Error;

use common::TxnId;
use proptest::prelude::*;
use proptest::test_runner::{Config, RngAlgorithm, TestCaseError, TestRng, TestRunner};
use storage::StorageError;
use storage::buffer::BufferPool;
use storage::disk::DiskManager;
use storage::dwb::DoubleWriteBuffer;
use storage::heap::{MAX_SLOTS, MAX_TUPLE_SIZE, TableHeap};
use storage::page::PAGE_SIZE;
use storage::replacer::LruKReplacer;
use storage::wal::LogManager;

const TXN: TxnId = TxnId(0);

fn open_pool(pool_size: usize) -> Result<(BufferPool, tempfile::TempDir), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let disk = DiskManager::open(dir.path().join("test.db"), PAGE_SIZE)?;
    let dwb = DoubleWriteBuffer::open(
        dir.path().join("test.db.dwb"),
        DoubleWriteBuffer::DEFAULT_CAPACITY,
    )?;
    let log = LogManager::open(dir.path().join("test.db.wal"))?;
    let replacer = Box::new(LruKReplacer::new(pool_size, 2));
    Ok((BufferPool::new(disk, dwb, log, pool_size, replacer), dir))
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

/// A page allocated but never `SlottedPage::init`-ed (as happens when a
/// process exits before flushing a freshly-created page) stays all-zero.
/// `NO_NEXT_PAGE` being `PageId(0)` - the one page id a heap chain can
/// never legitimately contain - means that decodes as a valid, empty,
/// terminal page rather than corruption: an uninitialized page is usable,
/// not merely non-fatal.
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

/// Proves an uninitialized (all-zero) page is genuinely usable, not just
/// safe to read: inserting into a heap whose only page was never
/// `SlottedPage::init`-ed must succeed, and the tuple must read back.
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

/// `MAX_SLOTS` remains a backstop against a page whose slot count could
/// never have come from `SlottedPage::insert` (a non-heap page misread as
/// one, say) - written directly here since `SlottedPage::insert` itself
/// can't be driven past it.
#[test]
fn slot_count_above_max_slots_still_reports_corruption() -> Result<(), Box<dyn Error>> {
    let (pool, _dir) = open_pool(4)?;
    let (page_id, mut guard) = pool.new_page(TXN)?;
    // Byte 12 is the start of the slotted-page header (the checksum and
    // `page_lsn` occupy the reserved `0..12` prefix; see
    // `heap::SLOT_COUNT_RANGE`).
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

/// No sequence of page bytes - however implausible - may panic a reader.
/// Installs 4096 fully random bytes as a heap page's raw content and drives
/// every public entry point that parses page bytes through it: iterating
/// the heap, looking up every possible slot by `Rid`, and inserting a
/// tuple. This is the property the original all-zero-page bug was really
/// an instance of - no on-disk byte pattern, whether from a lost write, a
/// torn write, or outright disk corruption, should be able to crash the
/// reader; proptest's own panic-catching (needed to shrink counterexamples)
/// is what turns any surviving panic into a normal, reproducible test
/// failure here rather than an aborted test binary. The RNG is seeded so a
/// failure is reproducible across runs.
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

        // Bounded: an adversarial `next_page_id` that happens to loop back
        // on itself would otherwise iterate forever rather than panic;
        // astronomically unlikely from random bytes, but the bound keeps
        // that outcome from hanging the test either way.
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
