//! Buffer pool behavior: eviction under pressure, dirty data surviving a
//! flush and refetch, pinning every frame forcing a clean error instead of
//! a panic, and the pin-count protocol itself (a page stays resident for as
//! long as *any* `PageGuard` on it is alive, and only becomes evictable
//! once the last one drops).

use std::error::Error;

use common::TxnId;
use storage::StorageError;
use storage::buffer::BufferPool;
use storage::disk::DiskManager;
use storage::page::PAGE_SIZE;
use storage::replacer::LruKReplacer;
use storage::wal::LogManager;

/// An arbitrary offset past the reserved checksum and `page_lsn` prefix
/// (`0..12`), used by these tests as a one-byte marker location. What byte
/// of the page it is doesn't matter to the buffer pool; it only matters
/// that it isn't the `page_lsn` field, which `PageGuard::write` overwrites
/// on every write.
const MARKER_OFFSET: usize = 100;

fn open_pool(pool_size: usize) -> Result<(BufferPool, tempfile::TempDir), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let disk = DiskManager::open(dir.path().join("test.db"), PAGE_SIZE)?;
    let log = LogManager::open(dir.path().join("test.db.wal"))?;
    let replacer = Box::new(LruKReplacer::new(pool_size, 2));
    Ok((BufferPool::new(disk, log, pool_size, replacer), dir))
}

/// Like `open_pool`, but with `k = 1` so the replacer's victim choice is
/// plain LRU (oldest access wins) instead of LRU-K's "prefer evicting
/// frames with less than `k` accesses" heuristic, which otherwise makes
/// eviction order hard to predict in a short-lived test.
fn open_pool_lru(pool_size: usize) -> Result<(BufferPool, tempfile::TempDir), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let disk = DiskManager::open(dir.path().join("test.db"), PAGE_SIZE)?;
    let log = LogManager::open(dir.path().join("test.db.wal"))?;
    let replacer = Box::new(LruKReplacer::new(pool_size, 1));
    Ok((BufferPool::new(disk, log, pool_size, replacer), dir))
}

/// Allocates and immediately releases `n` throwaway pages, to apply
/// eviction pressure on whatever else is currently unpinned in the pool.
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
        // `guard` drops here, unpinning the page so later iterations can
        // evict this frame once the pool's 4 frames are all in use.
    }

    // The first page written was necessarily evicted at least once (only
    // 4 frames for 10 pages); its dirty byte must have been flushed to
    // disk before eviction and come back correctly on refetch.
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
fn second_live_guard_keeps_page_pinned_under_eviction_pressure() -> Result<(), Box<dyn Error>> {
    let (pool, _dir) = open_pool_lru(2)?;

    let (p, mut guard1) = pool.new_page(TxnId(0))?;
    guard1.write(TxnId(0), MARKER_OFFSET, &[42])?;
    let guard2 = pool.fetch_page(p)?;

    // Releasing the first guard must not release the pin held by the
    // second: `PageGuard` is the sole owner of its own pin, not a shared
    // handle to "the page is pinned or not".
    drop(guard1);

    // Enough throwaway allocations to cycle every other frame in the pool
    // through eviction several times over.
    apply_pressure(&pool, 20)?;

    assert_eq!(
        guard2.page().data()[MARKER_OFFSET],
        42,
        "page P must not have been evicted while guard2 was still alive"
    );
    drop(guard2);
    Ok(())
}

#[test]
fn pin_count_reaches_zero_only_after_the_last_guard_drops() -> Result<(), Box<dyn Error>> {
    const N: usize = 4;
    let (pool, _dir) = open_pool_lru(2)?;

    let (p, mut first) = pool.new_page(TxnId(0))?;
    first.write(TxnId(0), MARKER_OFFSET, &[7])?;
    let mut guards = vec![first];
    for _ in 1..N {
        guards.push(pool.fetch_page(p)?);
    }
    assert_eq!(guards.len(), N);

    // Drop guards one at a time; after each drop except the last, at least
    // one guard is still alive, so P must survive eviction pressure.
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

    // Dropping the final guard does release the pin: P becomes evictable
    // and a fresh round of pressure is free to reclaim its frame.
    guards.clear();
    apply_pressure(&pool, 5)?;
    Ok(())
}
