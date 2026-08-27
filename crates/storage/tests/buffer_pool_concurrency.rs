use std::error::Error;
use std::sync::Arc;
use std::thread;

use common::TxnId;
use storage::buffer::BufferPool;
use storage::disk::DiskManager;
use storage::dwb::DoubleWriteBuffer;
use storage::page::PAGE_SIZE;
use storage::replacer::LruKReplacer;
use storage::wal::LogManager;

const MARKER_OFFSET: usize = 100;
const POOL_SIZE: usize = 4;
const PAGE_COUNT: usize = 20;
const THREAD_COUNT: usize = 8;
const ITERATIONS_PER_THREAD: usize = 200;

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
