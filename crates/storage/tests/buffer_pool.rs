//! Buffer pool behavior: eviction under pressure, dirty data surviving a
//! flush and refetch, and pinning every frame forcing a clean error instead
//! of a panic.

use std::error::Error;

use storage::StorageError;
use storage::buffer::BufferPool;
use storage::disk::DiskManager;
use storage::page::PAGE_SIZE;
use storage::replacer::LruKReplacer;

fn open_pool(pool_size: usize) -> Result<(BufferPool, tempfile::TempDir), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let disk = DiskManager::open(dir.path().join("test.db"), PAGE_SIZE)?;
    let replacer = Box::new(LruKReplacer::new(pool_size, 2));
    Ok((BufferPool::new(disk, pool_size, replacer), dir))
}

#[test]
fn evicts_under_pressure_and_dirty_data_survives_flush_and_refetch() -> Result<(), Box<dyn Error>> {
    let (pool, _dir) = open_pool(4)?;

    let mut page_ids = Vec::new();
    for marker in 0u8..10 {
        let (page_id, mut guard) = pool.new_page()?;
        guard.page_mut().data_mut()[0] = marker;
        page_ids.push(page_id);
        // `guard` drops here, unpinning the page so later iterations can
        // evict this frame once the pool's 4 frames are all in use.
    }

    // The first page written was necessarily evicted at least once (only
    // 4 frames for 10 pages); its dirty byte must have been flushed to
    // disk before eviction and come back correctly on refetch.
    let guard = pool.fetch_page(page_ids[0])?;
    assert_eq!(guard.page().data()[0], 0);
    drop(guard);
    pool.unpin_page(page_ids[0], false)?;

    pool.flush_all()?;

    let guard = pool.fetch_page(page_ids[9])?;
    assert_eq!(guard.page().data()[0], 9);
    drop(guard);
    pool.unpin_page(page_ids[9], false)?;

    Ok(())
}

#[test]
fn fetch_errors_rather_than_panics_when_every_frame_is_pinned() -> Result<(), Box<dyn Error>> {
    let (pool, _dir) = open_pool(2)?;

    let (_id_a, guard_a) = pool.new_page()?;
    let (_id_b, guard_b) = pool.new_page()?;

    match pool.new_page() {
        Err(StorageError::BufferPoolExhausted) => {}
        Err(other) => panic!("expected BufferPoolExhausted, got {other}"),
        Ok(_) => panic!("expected an error when every frame is pinned"),
    }

    drop(guard_a);
    drop(guard_b);
    Ok(())
}
