//! `TableHeap` behavior: inserting enough tuples to span several pages and
//! reading them all back, and a tuple too large for any page producing a
//! clear error instead of a panic (or an infinite loop).

use std::collections::HashMap;
use std::error::Error;

use storage::StorageError;
use storage::buffer::BufferPool;
use storage::disk::DiskManager;
use storage::heap::{MAX_TUPLE_SIZE, TableHeap};
use storage::page::PAGE_SIZE;
use storage::replacer::LruKReplacer;

fn open_pool(pool_size: usize) -> Result<(BufferPool, tempfile::TempDir), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let disk = DiskManager::open(dir.path().join("test.db"), PAGE_SIZE)?;
    let replacer = Box::new(LruKReplacer::new(pool_size, 2));
    Ok((BufferPool::new(disk, pool_size, replacer), dir))
}

#[test]
fn insert_spans_pages_and_iterates_every_tuple_back() -> Result<(), Box<dyn Error>> {
    let (pool, _dir) = open_pool(4)?;
    let mut heap = TableHeap::create(&pool)?;

    let payload = vec![0x42u8; 300];
    let mut expected: HashMap<common::Rid, Vec<u8>> = HashMap::new();
    for i in 0..40u32 {
        let mut tuple = payload.clone();
        tuple.extend_from_slice(&i.to_le_bytes());
        let rid = heap.insert_tuple(&tuple)?;
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
    let mut heap = TableHeap::create(&pool)?;

    let oversized = vec![0u8; MAX_TUPLE_SIZE + 1];
    match heap.insert_tuple(&oversized) {
        Err(StorageError::TupleTooLarge { .. }) => {}
        Err(other) => panic!("expected TupleTooLarge, got {other}"),
        Ok(_) => panic!("an oversized tuple must not be accepted"),
    }

    Ok(())
}

#[test]
fn get_tuple_returns_none_for_deleted_slot() -> Result<(), Box<dyn Error>> {
    let (pool, _dir) = open_pool(4)?;
    let mut heap = TableHeap::create(&pool)?;

    let rid = heap.insert_tuple(b"gone soon")?;
    heap.delete_tuple(rid)?;

    assert_eq!(heap.get_tuple(rid)?, None);
    Ok(())
}

/// Regression test: a page allocated but never `SlottedPage::init`-ed (as
/// happens when a process exits before flushing a freshly-created page)
/// stays all-zero, so its `next_page_id` field decodes as page 0 - a real
/// page id - rather than the `NO_NEXT_PAGE` sentinel. A scan must report
/// that as corruption instead of misreading the page-0 file header as
/// slotted-page bytes and running off the end of the page.
#[test]
fn scanning_a_page_whose_init_never_reached_disk_reports_corruption() -> Result<(), Box<dyn Error>>
{
    let (pool, _dir) = open_pool(4)?;
    let (page_id, guard) = pool.new_page()?;
    drop(guard);

    let heap = TableHeap::open(&pool, page_id);
    match heap.iter().next() {
        Some(Err(StorageError::CorruptPage { page_id: offending, .. })) => {
            assert_eq!(offending, 0, "the header page should be identified as the corrupt one");
        }
        other => panic!(
            "expected a CorruptPage error naming page 0, got a different outcome: {}",
            match other {
                Some(Ok(_)) => "a tuple",
                Some(Err(_)) => "a different error",
                None => "a clean end of heap",
            }
        ),
    }
    Ok(())
}
