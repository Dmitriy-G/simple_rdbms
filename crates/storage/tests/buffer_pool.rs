use std::error::Error;
use std::sync::{Barrier, mpsc};
use std::thread;
use std::time::Duration;

use common::TxnId;
use storage::StorageError;
use storage::buffer::BufferPool;
use storage::disk::DiskManager;
use storage::dwb::DoubleWriteBuffer;
use storage::page::PAGE_SIZE;
use storage::replacer::LruKReplacer;
use storage::wal::LogManager;

const MARKER_OFFSET: usize = 100;

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

fn open_pool_lru(pool_size: usize) -> Result<(BufferPool, tempfile::TempDir), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let disk = DiskManager::open(dir.path().join("test.db"), PAGE_SIZE)?;
    let dwb = DoubleWriteBuffer::open(
        dir.path().join("test.db.dwb"),
        DoubleWriteBuffer::DEFAULT_CAPACITY,
    )?;
    let log = LogManager::open(dir.path().join("test.db.wal"))?;
    let replacer = Box::new(LruKReplacer::new(pool_size, 1));
    Ok((BufferPool::new(disk, dwb, log, pool_size, replacer), dir))
}

fn apply_pressure(pool: &BufferPool, n: u8) -> Result<(), Box<dyn Error>> {
    for marker in 0..n {
        let (_id, mut guard) = pool.new_page(TxnId(0))?;
        guard.write(TxnId(0), MARKER_OFFSET, &[marker])?;
    }
    Ok(())
}

#[test]
fn evicts_under_pressure_and_dirty_data_survives_flush_and_refetch() -> Result<(), Box<dyn Error>> {
    let (pool, _dir) = open_pool(4)?;

    let mut page_ids = Vec::new();
    for marker in 0u8..10 {
        let (page_id, mut guard) = pool.new_page(TxnId(0))?;
        guard.write(TxnId(0), MARKER_OFFSET, &[marker])?;
        page_ids.push(page_id);
    }

    let guard = pool.fetch_page(page_ids[0])?;
    assert_eq!(guard.page().data()[MARKER_OFFSET], 0);
    drop(guard);

    pool.flush_all()?;

    let guard = pool.fetch_page(page_ids[9])?;
    assert_eq!(guard.page().data()[MARKER_OFFSET], 9);
    drop(guard);

    Ok(())
}

#[test]
fn fetch_errors_rather_than_panics_when_every_frame_is_pinned() -> Result<(), Box<dyn Error>> {
    let (pool, _dir) = open_pool(2)?;

    let (_id_a, guard_a) = pool.new_page(TxnId(0))?;
    let (_id_b, guard_b) = pool.new_page(TxnId(0))?;

    match pool.new_page(TxnId(0)) {
        Err(StorageError::BufferPoolExhausted) => {}
        Err(other) => panic!("expected BufferPoolExhausted, got {other}"),
        Ok(_) => panic!("expected an error when every frame is pinned"),
    }

    drop(guard_a);
    drop(guard_b);
    Ok(())
}

#[test]
fn second_live_read_guard_keeps_page_pinned_under_eviction_pressure() -> Result<(), Box<dyn Error>>
{
    let (pool, _dir) = open_pool_lru(2)?;

    let (p, mut guard1) = pool.new_page(TxnId(0))?;
    guard1.write(TxnId(0), MARKER_OFFSET, &[42])?;
    drop(guard1);

    let read_guard1 = pool.fetch_page_read(p)?;
    let read_guard2 = pool.fetch_page_read(p)?;

    drop(read_guard1);

    apply_pressure(&pool, 20)?;

    assert_eq!(
        read_guard2.page().data()[MARKER_OFFSET],
        42,
        "page P must not have been evicted while read_guard2 was still alive"
    );
    drop(read_guard2);
    Ok(())
}

#[test]
fn pin_count_reaches_zero_only_after_the_last_read_guard_drops() -> Result<(), Box<dyn Error>> {
    const N: usize = 4;
    let (pool, _dir) = open_pool_lru(2)?;

    let (p, mut first) = pool.new_page(TxnId(0))?;
    first.write(TxnId(0), MARKER_OFFSET, &[7])?;
    drop(first);

    let mut guards = Vec::with_capacity(N);
    for _ in 0..N {
        guards.push(pool.fetch_page_read(p)?);
    }
    assert_eq!(guards.len(), N);

    while guards.len() > 1 {
        guards.remove(0);
        apply_pressure(&pool, 5)?;
        if let Some(last) = guards.last() {
            assert_eq!(
                last.page().data()[MARKER_OFFSET],
                7,
                "page P was evicted while {} guard(s) were still alive",
                guards.len()
            );
        }
    }

    guards.clear();
    apply_pressure(&pool, 5)?;
    Ok(())
}

#[test]
fn write_guard_blocks_until_concurrent_read_guard_drops() -> Result<(), Box<dyn Error>> {
    let (pool, _dir) = open_pool(4)?;
    let (p, guard) = pool.new_page(TxnId(0))?;
    drop(guard);

    let read_guard = pool.fetch_page_read(p)?;
    let barrier = Barrier::new(2);
    let (tx, rx) = mpsc::channel();

    thread::scope(|scope| {
        scope.spawn(|| {
            barrier.wait();
            let acquired = pool.fetch_page(p).is_ok();
            tx.send(acquired).expect("send result");
        });

        barrier.wait();
        assert_eq!(
            rx.recv_timeout(Duration::from_millis(200)),
            Err(mpsc::RecvTimeoutError::Timeout),
            "a write guard must not be granted while a read guard is still alive"
        );

        drop(read_guard);

        assert_eq!(
            rx.recv_timeout(Duration::from_secs(5)),
            Ok(true),
            "the write guard must be granted once the read guard drops"
        );
    });

    Ok(())
}

#[test]
#[cfg(debug_assertions)]
#[should_panic(expected = "self-deadlock")]
fn a_second_write_guard_on_the_same_page_from_one_thread_panics() {
    let (pool, _dir) = open_pool(2).expect("open pool");
    let (p, _first) = pool.new_page(TxnId(0)).expect("new page");
    let _second = pool.fetch_page(p);
}
