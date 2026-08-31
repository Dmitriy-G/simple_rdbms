use std::error::Error;
use std::io;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use common::{PageId, TxnId};
use storage::StorageError;
use storage::block_device::{BlockDevice, FileDevice};
use storage::page::PAGE_SIZE;
use test_support::{PoolOptions, open_file};

const TEST_TIMEOUT: Duration = Duration::from_secs(5);
const PAUSE_SAFETY_NET: Duration = Duration::from_secs(10);
const SHORT_FRAME_WAIT_TIMEOUT: Duration = Duration::from_millis(100);
const PAYLOAD_OFFSET: usize = 100;

fn recv_within<T>(rx: &mpsc::Receiver<T>, timeout: Duration, what: &str) -> T {
    rx.recv_timeout(timeout)
        .unwrap_or_else(|_| panic!("timed out after {timeout:?} waiting for {what}"))
}

struct FailOnceDevice {
    inner: Box<dyn BlockDevice>,
    write_count: AtomicUsize,
    fail_on_write: usize,
}

impl BlockDevice for FailOnceDevice {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        self.inner.read_at(offset, buf)
    }

    fn write_at(&self, offset: u64, buf: &[u8]) -> io::Result<()> {
        let n = self.write_count.fetch_add(1, Ordering::SeqCst) + 1;
        if n == self.fail_on_write {
            return Err(io::Error::other("injected one-shot write failure"));
        }
        self.inner.write_at(offset, buf)
    }

    fn set_len(&self, len: u64) -> io::Result<()> {
        self.inner.set_len(len)
    }

    fn sync_all(&self) -> io::Result<()> {
        self.inner.sync_all()
    }

    fn size(&self) -> io::Result<u64> {
        self.inner.size()
    }
}

struct PausingDevice {
    inner: Box<dyn BlockDevice>,
    target_offset: u64,
    reached: Mutex<Option<mpsc::SyncSender<()>>>,
    release: Mutex<mpsc::Receiver<()>>,
}

impl PausingDevice {
    fn new(
        inner: Box<dyn BlockDevice>,
        target_offset: u64,
        reached: mpsc::SyncSender<()>,
        release: mpsc::Receiver<()>,
    ) -> Self {
        Self {
            inner,
            target_offset,
            reached: Mutex::new(Some(reached)),
            release: Mutex::new(release),
        }
    }
}

impl BlockDevice for PausingDevice {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        self.inner.read_at(offset, buf)
    }

    fn write_at(&self, offset: u64, buf: &[u8]) -> io::Result<()> {
        if offset == self.target_offset {
            let mut reached = self.reached.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(tx) = reached.take() {
                drop(reached);
                let _ = tx.send(());
                let release = self.release.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                let _ = release.recv_timeout(PAUSE_SAFETY_NET);
            }
        }
        self.inner.write_at(offset, buf)
    }

    fn set_len(&self, len: u64) -> io::Result<()> {
        self.inner.set_len(len)
    }

    fn sync_all(&self) -> io::Result<()> {
        self.inner.sync_all()
    }

    fn size(&self) -> io::Result<u64> {
        self.inner.size()
    }
}

fn page_offset(page_id: PageId) -> u64 {
    page_id.0 as u64 * PAGE_SIZE as u64
}

#[test]
fn a_failed_eviction_does_not_leak_the_frame() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    const POOL_SIZE: usize = 1;

    drop(test_support::open_pool(dir.path(), PoolOptions::new(POOL_SIZE))?);

    let db_file = open_file(&dir.path().join("test.db"))?;
    let data_device: Box<dyn BlockDevice> = Box::new(FailOnceDevice {
        inner: Box::new(FileDevice::new(db_file)),
        write_count: AtomicUsize::new(0),
        fail_on_write: 1,
    });
    let pool =
        test_support::open_pool(dir.path(), PoolOptions::new(POOL_SIZE).data_device(data_device))?;

    let (_p0, guard) = pool.new_page(TxnId(0))?;
    drop(guard);

    let evict_result = pool.new_page(TxnId(1));
    assert!(
        evict_result.is_err(),
        "the injected one-shot fault must fail the real page write that evicting the only \
         resident frame requires"
    );

    pool.assert_frame_accounting();

    for i in 0..POOL_SIZE + 2 {
        match pool.new_page(TxnId(2 + i as u64)) {
            Err(StorageError::FlushPoisoned) => {}
            Err(other) => panic!(
                "fetch {i} after the failed eviction must fail with FlushPoisoned once the pool \
                 is poisoned, got a different error instead: {other}"
            ),
            Ok((_, guard)) => {
                drop(guard);
                panic!(
                    "fetch {i} after the failed eviction must fail with FlushPoisoned once the \
                     pool is poisoned, but it succeeded instead - which is exactly what would \
                     let a later flush overwrite the double-write backup recovery still needs"
                );
            }
        }
    }
    pool.assert_frame_accounting();
    Ok(())
}

#[test]
fn a_write_during_a_flush_leaves_the_frame_dirty() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    const POOL_SIZE: usize = 2;

    let page_id = {
        let pool = test_support::open_pool(dir.path(), PoolOptions::new(POOL_SIZE))?;
        let (page_id, mut guard) = pool.new_page(TxnId(0))?;
        guard.write(TxnId(0), PAYLOAD_OFFSET, &[1])?;
        drop(guard);
        pool.flush_all()?;
        page_id
    };

    let target_offset = page_offset(page_id);
    let db_file = open_file(&dir.path().join("test.db"))?;
    let (reached_tx, reached_rx) = mpsc::sync_channel::<()>(0);
    let (release_tx, release_rx) = mpsc::sync_channel::<()>(0);
    let data_device: Box<dyn BlockDevice> = Box::new(PausingDevice::new(
        Box::new(FileDevice::new(db_file)),
        target_offset,
        reached_tx,
        release_rx,
    ));
    let pool = Arc::new(test_support::open_pool(
        dir.path(),
        PoolOptions::new(POOL_SIZE).data_device(data_device),
    )?);

    let mut guard = pool.fetch_page(page_id)?;
    guard.write(TxnId(1), PAYLOAD_OFFSET, &[2])?;
    drop(guard);

    let (flush_tx, flush_rx) = mpsc::channel();
    {
        let pool = Arc::clone(&pool);
        thread::spawn(move || {
            let result = pool.flush_page(page_id);
            let _ = flush_tx.send(result);
        });
    }

    recv_within(&reached_rx, TEST_TIMEOUT, "the flush's real write to reach the paused offset");

    let mut guard = pool.fetch_page(page_id)?;
    guard.write(TxnId(2), PAYLOAD_OFFSET, &[3])?;
    drop(guard);

    release_tx.send(()).ok();
    recv_within(&flush_rx, TEST_TIMEOUT, "the paused flush to finish")?;

    assert!(
        pool.dirty_page_table().iter().any(|&(id, _)| id == page_id),
        "page {page_id:?} must still be dirty: a second write landed on it after the flush had \
         already snapshotted the first write's bytes"
    );

    pool.flush_all()?;
    pool.sync()?;
    drop(pool);

    let reopened = test_support::open_pool(dir.path(), PoolOptions::new(POOL_SIZE))?;
    let guard = reopened.fetch_page_read(page_id)?;
    assert_eq!(
        guard.page().data()[PAYLOAD_OFFSET],
        3,
        "the second write must be durable after reopen, not lost under the first write's stale \
         flush"
    );
    Ok(())
}

#[test]
fn a_fetch_during_an_eviction_flush_never_sees_the_stale_page() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    const POOL_SIZE: usize = 1;

    let page_id = {
        let pool = test_support::open_pool(dir.path(), PoolOptions::new(POOL_SIZE))?;
        let (page_id, guard) = pool.new_page(TxnId(0))?;
        drop(guard);
        pool.flush_all()?;
        page_id
    };

    let target_offset = page_offset(page_id);
    let db_file = open_file(&dir.path().join("test.db"))?;
    let (reached_tx, reached_rx) = mpsc::sync_channel::<()>(0);
    let (release_tx, release_rx) = mpsc::sync_channel::<()>(0);
    let data_device: Box<dyn BlockDevice> = Box::new(PausingDevice::new(
        Box::new(FileDevice::new(db_file)),
        target_offset,
        reached_tx,
        release_rx,
    ));
    let pool = Arc::new(test_support::open_pool(
        dir.path(),
        PoolOptions::new(POOL_SIZE).data_device(data_device),
    )?);

    let mut guard = pool.fetch_page(page_id)?;
    guard.write(TxnId(1), 0, &[9])?;
    let dirty_lsn = guard.page().page_lsn();
    drop(guard);

    let (evict_tx, evict_rx) = mpsc::channel();
    {
        let pool = Arc::clone(&pool);
        thread::spawn(move || {
            let result = pool.new_page(TxnId(2)).map(|(id, guard)| {
                drop(guard);
                id
            });
            let _ = evict_tx.send(result);
        });
    }

    recv_within(&reached_rx, TEST_TIMEOUT, "the eviction's flush to reach the paused write");

    let (read_tx, read_rx) = mpsc::channel();
    {
        let pool = Arc::clone(&pool);
        thread::spawn(move || {
            let result = pool.fetch_page_read(page_id).map(|guard| guard.page().page_lsn());
            let _ = read_tx.send(result);
        });
    }

    release_tx.send(()).ok();

    let observed_lsn = recv_within(&read_rx, TEST_TIMEOUT, "the blocked fetch to resolve")?;
    assert_eq!(
        observed_lsn, dirty_lsn,
        "a fetch racing an in-flight eviction must observe the page's dirtied content once it \
         resolves, never a stale on-disk copy read before the eviction started"
    );

    recv_within(&evict_rx, TEST_TIMEOUT, "the eviction to finish")?;

    pool.assert_frame_accounting();
    Ok(())
}

#[test]
fn a_fetch_blocked_on_a_stuck_eviction_times_out_rather_than_hanging() -> Result<(), Box<dyn Error>>
{
    let dir = tempfile::tempdir()?;
    const POOL_SIZE: usize = 1;

    let page_id = {
        let pool = test_support::open_pool(dir.path(), PoolOptions::new(POOL_SIZE))?;
        let (page_id, guard) = pool.new_page(TxnId(0))?;
        drop(guard);
        pool.flush_all()?;
        page_id
    };

    let target_offset = page_offset(page_id);
    let db_file = open_file(&dir.path().join("test.db"))?;
    let (reached_tx, reached_rx) = mpsc::sync_channel::<()>(0);
    let (release_tx, release_rx) = mpsc::sync_channel::<()>(0);
    let data_device: Box<dyn BlockDevice> = Box::new(PausingDevice::new(
        Box::new(FileDevice::new(db_file)),
        target_offset,
        reached_tx,
        release_rx,
    ));
    let pool =
        test_support::open_pool(dir.path(), PoolOptions::new(POOL_SIZE).data_device(data_device))?;
    let pool = Arc::new(pool.with_frame_wait_timeout(SHORT_FRAME_WAIT_TIMEOUT));

    let mut guard = pool.fetch_page(page_id)?;
    guard.write(TxnId(1), 0, &[7])?;
    drop(guard);

    let (evict_tx, evict_rx) = mpsc::channel();
    {
        let pool = Arc::clone(&pool);
        thread::spawn(move || {
            let result = pool.new_page(TxnId(2)).map(|(id, guard)| {
                drop(guard);
                id
            });
            let _ = evict_tx.send(result);
        });
    }

    recv_within(&reached_rx, TEST_TIMEOUT, "the eviction's flush to reach the paused write");

    let started = std::time::Instant::now();
    let fetch_result = pool.fetch_page_read(page_id);
    let elapsed = started.elapsed();

    match &fetch_result {
        Err(StorageError::BufferPoolWaitTimedOut { .. }) => {}
        Err(other) => panic!(
            "a fetch blocked on a page stuck mid-eviction must time out with \
             BufferPoolWaitTimedOut, got a different error instead: {other}"
        ),
        Ok(_) => panic!(
            "a fetch blocked on a page stuck mid-eviction must time out with \
             BufferPoolWaitTimedOut, but it succeeded instead"
        ),
    }
    assert!(
        elapsed < SHORT_FRAME_WAIT_TIMEOUT * 10,
        "the timeout took {elapsed:?}, far longer than the configured {SHORT_FRAME_WAIT_TIMEOUT:?} \
         window - a blocked fetch must fail promptly, not merely eventually"
    );

    release_tx.send(()).ok();
    recv_within(&evict_rx, TEST_TIMEOUT, "the eviction to finish")?;

    pool.assert_frame_accounting();
    Ok(())
}
