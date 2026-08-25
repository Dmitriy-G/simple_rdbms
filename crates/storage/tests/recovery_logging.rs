use std::io::Write;
use std::sync::{Arc, Mutex};

use common::TxnId;
use storage::buffer::BufferPool;
use storage::disk::DiskManager;
use storage::dwb::DoubleWriteBuffer;
use storage::page::PAGE_SIZE;
use storage::recovery;
use storage::replacer::LruKReplacer;
use storage::wal::{LogManager, LogRecordKind};

#[derive(Clone, Default)]
struct CaptureBuf(Arc<Mutex<Vec<u8>>>);

#[cfg(test)]
impl Write for CaptureBuf {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
fn captured_events(buf: &CaptureBuf) -> Vec<serde_json::Value> {
    let bytes = buf.0.lock().unwrap();
    String::from_utf8_lossy(&bytes)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("captured line is valid JSON"))
        .collect()
}

#[test]
fn recovery_summary_event_reports_winners_losers_and_record_count() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let db_path = dir.path().join("test.db");
    let wal_path = dir.path().join("test.db.wal");
    let dwb_path = dir.path().join("test.db.dwb");

    {
        let disk = DiskManager::open(&db_path, PAGE_SIZE).expect("open disk");
        let dwb = DoubleWriteBuffer::open(&dwb_path, DoubleWriteBuffer::DEFAULT_CAPACITY)
            .expect("open dwb");
        let log = LogManager::open(&wal_path).expect("open log");
        let pool = BufferPool::new(disk, dwb, log, 8, Box::new(LruKReplacer::new(8, 2)));

        let (_, mut guard) = pool.new_page(TxnId(1)).expect("allocate page for winner");
        guard.write(TxnId(1), 20, b"winner").expect("write winner");
        drop(guard);
        pool.append_log(TxnId(1), LogRecordKind::Commit).expect("commit winner");

        let (_, mut guard) = pool.new_page(TxnId(2)).expect("allocate page for loser");
        guard.write(TxnId(2), 20, b"loser!").expect("write loser");
        drop(guard);
        pool.flush_log_all().expect("flush log");
    }

    let capture = CaptureBuf::default();
    let subscriber = tracing_subscriber::fmt()
        .json()
        .with_max_level(tracing::Level::INFO)
        .with_writer({
            let capture = capture.clone();
            move || capture.clone()
        })
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let disk = DiskManager::open(&db_path, PAGE_SIZE).expect("reopen disk");
    let dwb = DoubleWriteBuffer::open(&dwb_path, DoubleWriteBuffer::DEFAULT_CAPACITY)
        .expect("reopen dwb");
    let log = LogManager::open(&wal_path).expect("reopen log");
    let pool = BufferPool::new(disk, dwb, log, 8, Box::new(LruKReplacer::new(8, 2)));
    recovery::recover(&pool).expect("recover");

    let events = captured_events(&capture);
    let summary = events
        .iter()
        .find(|event| event["fields"]["message"] == "recovery complete")
        .expect("a recovery complete event was logged");

    assert_eq!(summary["fields"]["winners"], 1);
    assert_eq!(summary["fields"]["losers"], 1);
    assert!(
        summary["fields"]["records_scanned"].as_u64().expect("records_scanned is a number") > 0
    );
    assert!(summary["fields"]["duration_ms"].is_number());
}
