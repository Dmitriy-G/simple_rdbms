use std::error::Error;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use common::{DbConfig, Error as CommonError, SqlState};
use engine::{Database, ResultSet};
use storage::block_device::{BlockDevice, FileDevice};
use storage::wal::{FileSegmentStore, SegmentStore};

mod support;
use support::{CaptureBuf, captured_events, set_capturing_subscriber};

fn row_count(result: ResultSet) -> usize {
    match result {
        ResultSet::Rows { rows, .. } => rows.len(),
        ResultSet::RowsAffected(n) => panic!("expected Rows, got RowsAffected({n})"),
        ResultSet::RolledBack => panic!("expected Rows, got RolledBack"),
    }
}

fn short_lock_wait(dir: &Path) -> DbConfig {
    DbConfig { lock_wait_timeout_ms: 100, ..DbConfig::new(dir.join("test.db")) }
}

fn long_lock_wait(dir: &Path) -> DbConfig {
    DbConfig { lock_wait_timeout_ms: 5_000, ..DbConfig::new(dir.join("test.db")) }
}

#[test]
fn a_second_session_cannot_open_a_concurrent_transaction() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let mut db1 = Database::open(short_lock_wait(dir.path()))?;
    let mut db2 = db1.connect()?;

    db1.execute("BEGIN")?;

    match db2.execute("BEGIN") {
        Err(err @ CommonError::LockTimeout { .. }) => {
            assert_eq!(err.sql_state(), SqlState::LOCK_NOT_AVAILABLE);
        }
        Err(other) => panic!("expected LockTimeout, got {other}"),
        Ok(_) => panic!("a second session must not be able to open a concurrent transaction"),
    }
    Ok(())
}

#[test]
fn a_parked_begin_is_granted_when_the_holder_commits() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let mut db1 = Database::open(long_lock_wait(dir.path()))?;
    let db2 = db1.connect()?;

    db1.execute("BEGIN")?;

    let waiter = thread::spawn(move || {
        let mut db2 = db2;
        db2.execute("BEGIN")
    });

    thread::sleep(Duration::from_millis(100));
    db1.execute("COMMIT")?;

    let result = waiter.join().expect("waiter thread must not panic");
    assert!(result.is_ok(), "expected the parked BEGIN to be granted, got {result:?}");
    Ok(())
}

#[test]
fn a_parked_begin_fails_with_55p03_after_its_deadline() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let mut db1 = Database::open(short_lock_wait(dir.path()))?;
    let mut db2 = db1.connect()?;

    db1.execute("BEGIN")?;

    let start = std::time::Instant::now();
    match db2.execute("BEGIN") {
        Err(err @ CommonError::LockTimeout { .. }) => {
            assert_eq!(err.sql_state(), SqlState::LOCK_NOT_AVAILABLE);
        }
        Err(other) => panic!("expected LockTimeout, got {other}"),
        Ok(_) => panic!("the parked BEGIN must not succeed while the holder never resolves"),
    }
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "the parked BEGIN must fail at its own deadline, not hang"
    );
    Ok(())
}

#[test]
fn an_autocommit_statement_waits_for_an_open_transaction_to_finish() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let mut db1 = Database::open(long_lock_wait(dir.path()))?;
    let db2 = db1.connect()?;

    db1.execute("CREATE TABLE t (a INTEGER)")?;
    db1.execute("BEGIN")?;

    let waiter = thread::spawn(move || {
        let mut db2 = db2;
        db2.execute("INSERT INTO t VALUES (1)")
    });

    thread::sleep(Duration::from_millis(100));
    db1.execute("COMMIT")?;

    let result = waiter.join().expect("waiter thread must not panic");
    assert!(result.is_ok(), "expected the parked autocommit INSERT to be granted, got {result:?}");

    let rows = db1.execute("SELECT * FROM t")?;
    assert_eq!(row_count(rows), 1, "the granted autocommit INSERT must be visible");
    Ok(())
}

#[test]
fn dropping_a_session_mid_transaction_rolls_it_back() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let mut db1 = Database::open(DbConfig::new(dir.path().join("test.db")))?;
    db1.execute("CREATE TABLE t (a INTEGER)")?;

    let mut db2 = db1.connect()?;
    db2.execute("BEGIN")?;
    db2.execute("INSERT INTO t VALUES (1)")?;
    drop(db2);

    let rows = db1.execute("SELECT * FROM t")?;
    assert_eq!(row_count(rows), 0, "a session dropped mid-transaction must have its writes undone");
    Ok(())
}

fn open_file(path: &Path) -> std::io::Result<std::fs::File> {
    OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path)
}

fn wal_base_path(dir: &Path) -> PathBuf {
    let mut path = dir.join("test.db").into_os_string();
    path.push(".wal");
    PathBuf::from(path)
}

#[test]
fn dropping_a_session_mid_transaction_releases_the_wal_truncation_bound()
-> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let wal_segment_size = 4096u64;
    let config = DbConfig {
        checkpoint_byte_threshold: 512,
        buffer_pool_size: 4,
        ..DbConfig::new(dir.path().join("test.db"))
    };
    let db_device: Box<dyn BlockDevice> =
        Box::new(FileDevice::new(open_file(&dir.path().join("test.db"))?));
    let wal_store: Arc<dyn SegmentStore> =
        Arc::new(FileSegmentStore::new(wal_base_path(dir.path())));
    let dwb_device: Box<dyn BlockDevice> =
        Box::new(FileDevice::new(open_file(&dir.path().join("test.db.dwb"))?));
    let mut db1 =
        Database::open_with_devices(config, db_device, wal_store, wal_segment_size, dwb_device)?;

    db1.execute("CREATE TABLE t (a INTEGER, b TEXT)")?;
    let filler = "x".repeat(1800);

    let mut db2 = db1.connect()?;
    db2.execute("BEGIN")?;
    for i in 0..20 {
        db2.execute(&format!("INSERT INTO t VALUES ({i}, '{filler}')"))?;
    }

    let wal_base = wal_base_path(dir.path());
    let segments_while_held = FileSegmentStore::new(&wal_base).existing_segments()?;
    assert!(
        segments_while_held.len() > 1,
        "expected multiple WAL segments to exist by now, got {segments_while_held:?}"
    );
    let earliest_while_held = *segments_while_held.iter().min().expect("non-empty");

    drop(db2);

    for i in 20..40 {
        db1.execute(&format!("INSERT INTO t VALUES ({i}, '{filler}')"))?;
    }

    let segments_after = FileSegmentStore::new(&wal_base).existing_segments()?;
    let earliest_after = *segments_after.iter().min().expect("non-empty");

    assert!(
        earliest_after > earliest_while_held,
        "expected truncation to advance past the released bound; before={segments_while_held:?} \
         after={segments_after:?}"
    );
    Ok(())
}

#[test]
fn an_idle_in_transaction_session_is_aborted_after_the_timeout() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let config = DbConfig {
        idle_in_transaction_timeout_ms: 100,
        ..DbConfig::new(dir.path().join("test.db"))
    };
    let mut db = Database::open(config)?;
    db.execute("CREATE TABLE t (a INTEGER)")?;
    db.execute("BEGIN")?;

    thread::sleep(Duration::from_millis(400));

    match db.execute("SELECT * FROM t") {
        Err(err @ CommonError::IdleInTransactionTimeout) => {
            assert_eq!(err.sql_state(), SqlState::IDLE_IN_TRANSACTION_SESSION_TIMEOUT);
        }
        Err(other) => panic!("expected IdleInTransactionTimeout, got {other}"),
        Ok(_) => {
            panic!("expected the timed-out transaction's next statement to report the timeout")
        }
    }

    db.execute("BEGIN")?;
    db.execute("COMMIT")?;
    Ok(())
}

#[test]
fn a_dead_engine_thread_fails_every_session_with_a_fatal_error() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let mut db1 = Database::open(DbConfig::new(dir.path().join("test.db")))?;
    let mut db2 = db1.connect()?;

    db1.kill_engine_for_test();

    match db1.execute("CREATE TABLE t (a INTEGER)") {
        Err(CommonError::EngineUnavailable { .. }) => {}
        other => panic!("expected EngineUnavailable, got {other:?}"),
    }
    match db2.execute("CREATE TABLE t (a INTEGER)") {
        Err(CommonError::EngineUnavailable { .. }) => {}
        other => panic!("expected EngineUnavailable, got {other:?}"),
    }
    Ok(())
}

fn checkpoint_count(
    threshold: u64,
    drive: impl FnOnce(&mut Database, &[String]) -> Result<(), Box<dyn Error>>,
    statements: &[String],
) -> Result<usize, Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let capture = CaptureBuf::default();
    let _guard = set_capturing_subscriber(&capture);

    let config = DbConfig {
        checkpoint_byte_threshold: threshold,
        ..DbConfig::new(dir.path().join("test.db"))
    };
    let mut db = Database::open(config)?;
    db.execute("CREATE TABLE t (a INTEGER)")?;
    drive(&mut db, statements)?;

    let events = captured_events(&capture);
    Ok(events.iter().filter(|event| event["fields"]["message"] == "checkpoint complete").count())
}

#[test]
fn several_sessions_checkpoint_once_per_threshold_not_once_each() -> Result<(), Box<dyn Error>> {
    let statements: Vec<String> = (0..40).map(|i| format!("INSERT INTO t VALUES ({i})")).collect();

    let single_session = checkpoint_count(
        2048,
        |db, stmts| {
            for stmt in stmts {
                db.execute(stmt)?;
            }
            Ok(())
        },
        &statements,
    )?;

    let split_across_sessions = checkpoint_count(
        2048,
        |db1, stmts| {
            let mut db2 = db1.connect()?;
            for (i, stmt) in stmts.iter().enumerate() {
                if i % 2 == 0 {
                    db1.execute(stmt)?;
                } else {
                    db2.execute(stmt)?;
                }
            }
            Ok(())
        },
        &statements,
    )?;

    assert!(
        single_session > 0,
        "expected the generated log growth to trigger at least one checkpoint"
    );
    assert_eq!(
        single_session, split_across_sessions,
        "splitting identical statements across two sessions must not multiply the checkpoint count"
    );
    Ok(())
}
