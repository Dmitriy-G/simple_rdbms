#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::Write;
use std::sync::{Arc, Mutex};

use common::DbConfig;
use engine::Database;

#[derive(Clone, Default)]
struct CaptureBuf(Arc<Mutex<Vec<u8>>>);

impl Write for CaptureBuf {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn captured_events(buf: &CaptureBuf) -> Vec<serde_json::Value> {
    let bytes = buf.0.lock().unwrap();
    String::from_utf8_lossy(&bytes)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("captured line is valid JSON"))
        .collect()
}

fn set_capturing_subscriber(capture: &CaptureBuf) -> tracing::subscriber::DefaultGuard {
    let subscriber = tracing_subscriber::fmt()
        .json()
        .with_max_level(tracing::Level::TRACE)
        .with_writer({
            let capture = capture.clone();
            move || capture.clone()
        })
        .finish();
    tracing::subscriber::set_default(subscriber)
}

#[test]
fn a_successful_statement_is_logged_at_info_with_no_literal_values() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let capture = CaptureBuf::default();
    let _guard = set_capturing_subscriber(&capture);

    let mut db = Database::open(DbConfig::new(dir.path().join("test.db"))).expect("open");
    db.execute("CREATE TABLE t (a INTEGER, name TEXT)").expect("create table");
    db.execute("INSERT INTO t VALUES (42, 'alice-secret')").expect("insert");

    let events = captured_events(&capture);
    let info_line = events
        .iter()
        .find(|event| {
            event["level"] == "INFO"
                && event["fields"]["message"] == "statement executed"
                && event["fields"]["fingerprint"].as_str().unwrap_or("").contains("VALUES")
        })
        .expect("an info-level statement-executed event for the INSERT was logged");

    let fingerprint = info_line["fields"]["fingerprint"].as_str().expect("fingerprint is a string");
    assert!(!fingerprint.contains("42"), "literal value leaked into info log: {fingerprint}");
    assert!(
        !fingerprint.contains("alice-secret"),
        "literal value leaked into info log: {fingerprint}"
    );
    assert!(fingerprint.contains('?'), "expected redacted placeholders, got: {fingerprint}");
}

#[test]
fn the_transaction_spans_txn_id_reaches_an_event_logged_deep_in_storage() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let capture = CaptureBuf::default();
    let _guard = set_capturing_subscriber(&capture);

    let mut db = Database::open(DbConfig::new(dir.path().join("test.db"))).expect("open");
    db.execute("CREATE TABLE t (a INTEGER)").expect("create table");
    db.execute("BEGIN").expect("begin");
    db.execute("INSERT INTO t VALUES (1)").expect("insert");
    db.execute("COMMIT").expect("commit");

    let events = captured_events(&capture);
    let carries_txn_id =
        events.iter().filter(|event| event["fields"]["message"] == "fetch_page").any(|event| {
            event["spans"]
                .as_array()
                .map(|spans| spans.iter().any(|span| span.get("txn_id").is_some()))
                .unwrap_or(false)
        });

    assert!(
        carries_txn_id,
        "expected at least one storage::buffer::fetch_page trace event to carry the \
         transaction span's txn_id in its ancestor spans"
    );
}
