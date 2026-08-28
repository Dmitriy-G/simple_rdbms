use std::error::Error;
use std::sync::{Arc, Barrier};
use std::thread;

use common::TxnId;
use storage::buffer::BufferPool;
use storage::disk::DiskManager;
use storage::dwb::DoubleWriteBuffer;
use storage::page::PAGE_SIZE;
use storage::replacer::LruKReplacer;
use storage::wal::LogManager;

const MARKER_OFFSET: usize = 100;
const THREAD_COUNT: usize = 8;
const POOL_SIZE: usize = THREAD_COUNT + 1;
const PAGE_COUNT: usize = 20;
const ITERATIONS_PER_THREAD: usize = 200;

const COUNTER_OFFSET: usize = 200;
const RMW_PAGE_COUNT: usize = 3;
const RMW_THREAD_COUNT: usize = 8;
const RMW_POOL_SIZE: usize = RMW_THREAD_COUNT + 1;
const RMW_ROUNDS: usize = 200;

const RACE_POOL_SIZE: usize = 2;
const RACE_ITERATIONS: usize = 200;

fn read_counter(data: &[u8; PAGE_SIZE]) -> u32 {
    u32::from_le_bytes([
        data[COUNTER_OFFSET],
        data[COUNTER_OFFSET + 1],
        data[COUNTER_OFFSET + 2],
        data[COUNTER_OFFSET + 3],
    ])
}

#[test]
fn eight_threads_fetching_reading_and_releasing_never_see_torn_or_wrong_page_contents()
-> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let disk = DiskManager::open(dir.path().join("test.db"), PAGE_SIZE)?;
    let dwb = DoubleWriteBuffer::open(
        dir.path().join("test.db.dwb"),
        DoubleWriteBuffer::DEFAULT_CAPACITY,
    )?;
    let log = LogManager::open(dir.path().join("test.db.wal"))?;
    let replacer = Box::new(LruKReplacer::new(POOL_SIZE, 2));
    let pool = BufferPool::new(disk, dwb, log, POOL_SIZE, replacer);

    let mut page_ids = Vec::with_capacity(PAGE_COUNT);
    for marker in 0..PAGE_COUNT {
        let (page_id, mut guard) = pool.new_page(TxnId(0))?;
        guard.write(TxnId(0), MARKER_OFFSET, &[marker as u8])?;
        drop(guard);
        page_ids.push(page_id);
    }
    pool.flush_all()?;

    let pool = Arc::new(pool);
    let page_ids = Arc::new(page_ids);

    let handles: Vec<_> = (0..THREAD_COUNT)
        .map(|thread_index| {
            let pool = Arc::clone(&pool);
            let page_ids = Arc::clone(&page_ids);
            thread::spawn(move || -> Result<(), String> {
                for i in 0..ITERATIONS_PER_THREAD {
                    let slot = (thread_index * 37 + i) % page_ids.len();
                    let page_id = page_ids[slot];
                    let guard = pool.fetch_page_read(page_id).map_err(|err| err.to_string())?;
                    let expected = slot as u8;
                    let actual = guard.page().data()[MARKER_OFFSET];
                    if actual != expected {
                        return Err(format!(
                            "page {page_id:?} (slot {slot}): expected marker {expected}, got \
                             {actual}"
                        ));
                    }
                }
                Ok(())
            })
        })
        .collect();

    for handle in handles {
        handle.join().expect("worker thread panicked")?;
    }
    Ok(())
}

#[test]
fn concurrent_read_modify_write_cycles_never_lose_an_update() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let db_path = dir.path().join("test.db");
    let dwb_path = dir.path().join("test.db.dwb");
    let wal_path = dir.path().join("test.db.wal");

    let disk = DiskManager::open(&db_path, PAGE_SIZE)?;
    let dwb = DoubleWriteBuffer::open(&dwb_path, DoubleWriteBuffer::DEFAULT_CAPACITY)?;
    let log = LogManager::open(&wal_path)?;
    let replacer = Box::new(LruKReplacer::new(RMW_POOL_SIZE, 2));
    let pool = BufferPool::new(disk, dwb, log, RMW_POOL_SIZE, replacer);

    let mut page_ids = Vec::with_capacity(RMW_PAGE_COUNT);
    for _ in 0..RMW_PAGE_COUNT {
        let (page_id, guard) = pool.new_page(TxnId(0))?;
        drop(guard);
        page_ids.push(page_id);
    }
    pool.flush_all()?;

    let mut expected = [0u32; RMW_PAGE_COUNT];
    for round in 0..RMW_ROUNDS {
        for _ in 0..RMW_POOL_SIZE {
            let (_, guard) = pool.new_page(TxnId(0))?;
            drop(guard);
        }

        let barrier = Barrier::new(RMW_THREAD_COUNT);
        thread::scope(|scope| -> Result<(), String> {
            let handles: Vec<_> = (0..RMW_THREAD_COUNT)
                .map(|thread_index| {
                    let pool = &pool;
                    let page_ids = &page_ids;
                    let barrier = &barrier;
                    scope.spawn(move || -> Result<(), String> {
                        let txn_id = TxnId((round * RMW_THREAD_COUNT + thread_index) as u64 + 1);
                        let page_id = page_ids[(thread_index + round) % page_ids.len()];
                        barrier.wait();
                        let mut guard = pool.fetch_page(page_id).map_err(|err| err.to_string())?;
                        let current = read_counter(guard.page().data());
                        guard
                            .write(txn_id, COUNTER_OFFSET, &(current + 1).to_le_bytes())
                            .map_err(|err| err.to_string())?;
                        Ok(())
                    })
                })
                .collect();
            for handle in handles {
                handle.join().expect("worker thread panicked")?;
            }
            Ok(())
        })?;

        for thread_index in 0..RMW_THREAD_COUNT {
            expected[(thread_index + round) % RMW_PAGE_COUNT] += 1;
        }
    }

    pool.flush_all()?;
    drop(pool);

    let disk = DiskManager::open(&db_path, PAGE_SIZE)?;
    let dwb = DoubleWriteBuffer::open(&dwb_path, DoubleWriteBuffer::DEFAULT_CAPACITY)?;
    let log = LogManager::open(&wal_path)?;
    let replacer = Box::new(LruKReplacer::new(RMW_POOL_SIZE, 2));
    let reopened = BufferPool::new(disk, dwb, log, RMW_POOL_SIZE, replacer);

    for (slot, &page_id) in page_ids.iter().enumerate() {
        let guard = reopened.fetch_page_read(page_id)?;
        let actual = read_counter(guard.page().data());
        assert_eq!(
            actual, expected[slot],
            "page {page_id:?} (slot {slot}): expected {} increments, saw {actual}",
            expected[slot]
        );
    }
    Ok(())
}

#[test]
fn two_threads_racing_a_cold_page_install_yields_exactly_one_frame() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let disk = DiskManager::open(dir.path().join("test.db"), PAGE_SIZE)?;
    let dwb = DoubleWriteBuffer::open(
        dir.path().join("test.db.dwb"),
        DoubleWriteBuffer::DEFAULT_CAPACITY,
    )?;
    let log = LogManager::open(dir.path().join("test.db.wal"))?;
    let replacer = Box::new(LruKReplacer::new(RACE_POOL_SIZE, 2));
    let pool = BufferPool::new(disk, dwb, log, RACE_POOL_SIZE, replacer);

    let mut page_ids = Vec::with_capacity(RACE_ITERATIONS + RACE_POOL_SIZE);
    for _ in 0..RACE_ITERATIONS + RACE_POOL_SIZE {
        let (page_id, guard) = pool.new_page(TxnId(0))?;
        drop(guard);
        page_ids.push(page_id);
    }
    pool.flush_all()?;

    for &page_id in &page_ids[..RACE_ITERATIONS] {
        let barrier = Barrier::new(2);
        thread::scope(|scope| -> Result<(), String> {
            let handles: Vec<_> = (0..2)
                .map(|_| {
                    let pool = &pool;
                    let barrier = &barrier;
                    scope.spawn(move || -> Result<(), String> {
                        barrier.wait();
                        pool.fetch_page_read(page_id).map_err(|err| err.to_string())?;
                        Ok(())
                    })
                })
                .collect();
            for handle in handles {
                handle.join().expect("worker thread panicked")?;
            }
            Ok(())
        })?;

        assert_eq!(
            pool.frame_count_for(page_id),
            1,
            "page {page_id:?}: expected exactly one resident frame after the race"
        );
    }
    Ok(())
}
