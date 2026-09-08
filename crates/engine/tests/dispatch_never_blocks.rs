use std::collections::VecDeque;
use std::error::Error;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use common::DbConfig;
use engine::{Database, ResultSet};
use storage::block_device::{BlockDevice, FileDevice};
use storage::wal::{FileSegmentStore, SegmentStore};
use test_support::open_file;

const REACHED_TIMEOUT: Duration = Duration::from_secs(10);
const RESPONSIVENESS_BOUND: Duration = Duration::from_millis(500);
const PAUSE_SAFETY_NET: Duration = Duration::from_secs(10);

struct PauseSlot {
    reached: mpsc::SyncSender<()>,
    release: mpsc::Receiver<()>,
}

struct PausingDevice {
    inner: FileDevice,
    armed: Arc<AtomicBool>,
    slots: Mutex<VecDeque<PauseSlot>>,
}

impl PausingDevice {
    fn new(inner: FileDevice, armed: Arc<AtomicBool>, slots: VecDeque<PauseSlot>) -> Self {
        Self { inner, armed, slots: Mutex::new(slots) }
    }
}

impl BlockDevice for PausingDevice {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        if self.armed.swap(false, Ordering::SeqCst) {
            let mut slots = self.slots.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(slot) = slots.pop_front() {
                drop(slots);
                let _ = slot.reached.send(());
                let _ = slot.release.recv_timeout(PAUSE_SAFETY_NET);
            }
        }
        self.inner.read_at(offset, buf)
    }

    fn write_at(&self, offset: u64, buf: &[u8]) -> io::Result<()> {
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

fn recv_within<T>(rx: &mpsc::Receiver<T>, timeout: Duration, what: &str) -> T {
    rx.recv_timeout(timeout)
        .unwrap_or_else(|_| panic!("timed out after {timeout:?} waiting for {what}"))
}

fn row_count(result: ResultSet) -> usize {
    match result {
        ResultSet::Rows { rows, .. } => rows.len(),
        ResultSet::RowsAffected(n) => panic!("expected Rows, got RowsAffected({n})"),
        ResultSet::RolledBack => panic!("expected Rows, got RolledBack"),
    }
}

#[test]
fn a_paused_select_does_not_block_an_unrelated_sessions_request() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let config = DbConfig { buffer_pool_size: 2, ..DbConfig::new(dir.path().join("test.db")) };

    let armed = Arc::new(AtomicBool::new(false));
    let (scan_reached_tx, scan_reached_rx) = mpsc::sync_channel::<()>(0);
    let (scan_release_tx, scan_release_rx) = mpsc::sync_channel::<()>(0);
    let (reload_reached_tx, reload_reached_rx) = mpsc::sync_channel::<()>(0);
    let (reload_release_tx, reload_release_rx) = mpsc::sync_channel::<()>(0);
    let slots = VecDeque::from(vec![
        PauseSlot { reached: scan_reached_tx, release: scan_release_rx },
        PauseSlot { reached: reload_reached_tx, release: reload_release_rx },
    ]);
    let db_device: Box<dyn BlockDevice> = Box::new(PausingDevice::new(
        FileDevice::new(open_file(&dir.path().join("test.db"))?),
        Arc::clone(&armed),
        slots,
    ));
    let wal_store: Arc<dyn SegmentStore> =
        Arc::new(FileSegmentStore::new(wal_base_path(dir.path())));
    let dwb_device: Box<dyn BlockDevice> =
        Box::new(FileDevice::new(open_file(&dir.path().join("test.db.dwb"))?));
    let mut db1 = Database::open_with_devices(config, db_device, wal_store, 4096, dwb_device)?;

    db1.execute("CREATE TABLE t (a INTEGER, b TEXT)")?;
    let filler = "x".repeat(1800);
    for i in 0..10 {
        db1.execute(&format!("INSERT INTO t VALUES ({i}, '{filler}')"))?;
    }

    let mut db2 = db1.connect()?;
    db2.execute("BEGIN")?;

    armed.store(true, Ordering::SeqCst);
    let mut db_scan = db1.connect()?;
    let scan_handle = thread::spawn(move || db_scan.execute("SELECT * FROM t"));

    recv_within(&scan_reached_rx, REACHED_TIMEOUT, "the paused SELECT to reach its blocked read");

    armed.store(true, Ordering::SeqCst);
    drop(db2);
    recv_within(
        &reload_reached_rx,
        REACHED_TIMEOUT,
        "the disconnect's catalog reload to reach its blocked read, which is where it holds the \
         catalog's mutation lock",
    );

    let db_probe = db1.connect()?;
    let (probe_tx, probe_rx) = mpsc::channel();
    let probe_handle = thread::spawn(move || {
        let result = db_probe.table_names();
        let _ = probe_tx.send(result);
    });
    recv_within(
        &probe_rx,
        RESPONSIVENESS_BOUND,
        "an unrelated session's table_names() request while a long SELECT is paused and a \
         catalog reload is known to be parked mid-read holding the catalog's mutation lock",
    );
    probe_handle.join().expect("probe thread must not panic");

    let _ = reload_release_tx.send(());
    let _ = scan_release_tx.send(());
    let scan_result = scan_handle.join().expect("scan thread must not panic")?;
    assert_eq!(row_count(scan_result), 10, "the paused SELECT must still complete correctly");
    Ok(())
}

fn wal_base_path(dir: &std::path::Path) -> std::path::PathBuf {
    let mut path = dir.join("test.db").into_os_string();
    path.push(".wal");
    std::path::PathBuf::from(path)
}
