use common::DbConfig;
use engine::Database;
use test_support::{CaptureBuf, captured_events, set_capturing_subscriber};

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
fn a_failed_statement_is_logged_with_fingerprint_and_no_literal_values() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let capture = CaptureBuf::default();
    let _guard = set_capturing_subscriber(&capture);

    let mut db = Database::open(DbConfig::new(dir.path().join("test.db"))).expect("open");
    db.execute("CREATE TABLE t (amount INTEGER)").expect("create table");
    let result = db.execute("INSERT INTO t VALUES (99999999999)");
    assert!(result.is_err(), "expected the out-of-range literal to be rejected");

    let events = captured_events(&capture);
    let warn_line = events
        .iter()
        .find(|event| event["level"] == "WARN" && event["fields"]["message"] == "statement failed")
        .expect("a warn-level statement-failed event was logged");

    let error = warn_line["fields"]["error"].as_str().expect("error is a string");
    assert!(!error.contains("99999999999"), "literal value leaked into warn log: {error}");
    assert!(error.contains('?'), "expected a redacted placeholder, got: {error}");
    assert!(error.contains("amount"), "expected the column name preserved, got: {error}");

    let fingerprint = warn_line["fields"]["fingerprint"].as_str().expect("fingerprint is a string");
    assert!(
        !fingerprint.contains("99999999999"),
        "literal value leaked into warn log via fingerprint: {fingerprint}"
    );

    let debug_line = events
        .iter()
        .find(|event| event["level"] == "DEBUG" && event["fields"]["message"] == "statement failed")
        .expect("a debug-level statement-failed event with the full error was logged");
    let full_error = debug_line["fields"]["err"].as_str().expect("err is a string");
    assert!(
        full_error.contains("99999999999"),
        "expected the full literal at debug level, got: {full_error}"
    );
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
