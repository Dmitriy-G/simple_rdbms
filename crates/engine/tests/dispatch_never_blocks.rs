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
    read_armed: Arc<AtomicBool>,
    read_slots: Mutex<VecDeque<PauseSlot>>,
    write_armed: Arc<AtomicBool>,
    write_slots: Mutex<VecDeque<PauseSlot>>,
}

impl PausingDevice {
    fn new(
        inner: FileDevice,
        read_armed: Arc<AtomicBool>,
        read_slots: VecDeque<PauseSlot>,
    ) -> Self {
        Self {
            inner,
            read_armed,
            read_slots: Mutex::new(read_slots),
            write_armed: Arc::new(AtomicBool::new(false)),
            write_slots: Mutex::new(VecDeque::new()),
        }
    }

    fn with_write_pause(
        inner: FileDevice,
        write_armed: Arc<AtomicBool>,
        write_slots: VecDeque<PauseSlot>,
    ) -> Self {
        Self {
            inner,
            read_armed: Arc::new(AtomicBool::new(false)),
            read_slots: Mutex::new(VecDeque::new()),
            write_armed,
            write_slots: Mutex::new(write_slots),
        }
    }
}

fn pause_if_armed(armed: &AtomicBool, slots: &Mutex<VecDeque<PauseSlot>>) {
    if armed.swap(false, Ordering::SeqCst) {
        let mut slots = slots.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(slot) = slots.pop_front() {
            drop(slots);
            let _ = slot.reached.send(());
            let _ = slot.release.recv_timeout(PAUSE_SAFETY_NET);
        }
    }
}

impl BlockDevice for PausingDevice {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        pause_if_armed(&self.read_armed, &self.read_slots);
        self.inner.read_at(offset, buf)
    }

    fn write_at(&self, offset: u64, buf: &[u8]) -> io::Result<()> {
        pause_if_armed(&self.write_armed, &self.write_slots);
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

#[test]
fn a_stalled_checkpoint_does_not_block_an_unrelated_session() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let config = DbConfig::new(dir.path().join("test.db"));

    let write_armed = Arc::new(AtomicBool::new(false));
    let (flush_reached_tx, flush_reached_rx) = mpsc::sync_channel::<()>(0);
    let (flush_release_tx, flush_release_rx) = mpsc::sync_channel::<()>(0);
    let write_slots =
        VecDeque::from(vec![PauseSlot { reached: flush_reached_tx, release: flush_release_rx }]);
    let db_device: Box<dyn BlockDevice> = Box::new(PausingDevice::with_write_pause(
        FileDevice::new(open_file(&dir.path().join("test.db"))?),
        Arc::clone(&write_armed),
        write_slots,
    ));
    let wal_store: Arc<dyn SegmentStore> =
        Arc::new(FileSegmentStore::new(wal_base_path(dir.path())));
    let dwb_device: Box<dyn BlockDevice> =
        Box::new(FileDevice::new(open_file(&dir.path().join("test.db.dwb"))?));
    let db1 = Database::open_with_devices(config, db_device, wal_store, 4096, dwb_device)?;

    {
        let mut setup = db1.connect()?;
        setup.execute("CREATE TABLE t (a INTEGER)")?;
        setup.execute("INSERT INTO t VALUES (1)")?;
    }

    let db_probe_base = db1.connect()?;

    write_armed.store(true, Ordering::SeqCst);
    let checkpoint_handle = thread::spawn(move || db1.close());

    recv_within(
        &flush_reached_rx,
        REACHED_TIMEOUT,
        "the checkpoint's flush to reach its stalled page write",
    );

    let (probe_tx, probe_rx) = mpsc::channel();
    let probe_handle = thread::spawn(move || {
        let mut db_probe = db_probe_base.connect().expect("connect must not fail");
        let names = db_probe.table_names();
        let rows = db_probe.execute("SELECT * FROM t");
        let _ = probe_tx.send((names, rows));
        (db_probe_base, db_probe)
    });
    let (names, rows) = recv_within(
        &probe_rx,
        RESPONSIVENESS_BOUND,
        "an unrelated session's Connect, table_names() and SELECT while a checkpoint's page-0 \
         flush is parked mid-write inside the worker pool",
    );
    assert!(
        names.iter().any(|name| name == "t"),
        "the probe must get the real catalog back, not an error default; got {names:?}"
    );
    assert_eq!(
        row_count(rows?),
        1,
        "the probe's statement must run to completion, not merely be dispatched"
    );

    let _ = flush_release_tx.send(());
    checkpoint_handle.join().expect("checkpoint thread must not panic")?;
    let (_probe_base, _probe) = probe_handle.join().expect("probe thread must not panic");
    Ok(())
}

fn wal_base_path(dir: &std::path::Path) -> std::path::PathBuf {
    let mut path = dir.join("test.db").into_os_string();
    path.push(".wal");
    std::path::PathBuf::from(path)
}
