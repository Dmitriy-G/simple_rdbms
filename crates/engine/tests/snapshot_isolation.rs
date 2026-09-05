use std::error::Error;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use common::DbConfig;
use engine::Database;
use types::Value;

mod support;
use support::rows_of;

const CONCURRENT_TEST_TIMEOUT: Duration = Duration::from_secs(10);

fn recv_within<T>(rx: &mpsc::Receiver<T>, timeout: Duration, what: &str) -> T {
    rx.recv_timeout(timeout)
        .unwrap_or_else(|_| panic!("timed out after {timeout:?} waiting for {what}"))
}

#[test]
fn a_concurrent_select_does_not_block_behind_an_uncommitted_insert() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let mut db1 = Database::open(DbConfig::new(dir.path().join("test.db")))?;
    db1.execute("CREATE TABLE t (a INTEGER)")?;
    let mut db2 = db1.connect()?;

    db1.execute("BEGIN")?;
    db1.execute("INSERT INTO t VALUES (1)")?;

    let (tx, rx) = mpsc::channel();
    let handle = thread::spawn(move || {
        let result = db2.execute("SELECT * FROM t");
        let _ = tx.send(result);
    });

    let result = recv_within(&rx, CONCURRENT_TEST_TIMEOUT, "the concurrent SELECT to return");
    handle.join().expect("worker thread must not panic");
    assert_eq!(
        rows_of(result?),
        Vec::<Vec<Value>>::new(),
        "an uncommitted insert must not be visible to a concurrent session"
    );

    db1.execute("COMMIT")?;
    Ok(())
}

#[test]
fn a_readers_snapshot_does_not_move_after_another_sessions_commit() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let mut db1 = Database::open(DbConfig::new(dir.path().join("test.db")))?;
    db1.execute("CREATE TABLE t (a INTEGER)")?;
    let mut db2 = db1.connect()?;
    let mut db3 = db1.connect()?;

    db2.execute("BEGIN")?;

    db1.execute("BEGIN")?;
    db1.execute("INSERT INTO t VALUES (1)")?;
    db1.execute("COMMIT")?;

    assert_eq!(
        rows_of(db2.execute("SELECT * FROM t")?),
        Vec::<Vec<Value>>::new(),
        "a transaction begun before the commit must not see the row even after the commit lands"
    );
    db2.execute("ROLLBACK")?;

    db3.execute("BEGIN")?;
    assert_eq!(
        rows_of(db3.execute("SELECT * FROM t")?),
        vec![vec![Value::Integer(1)]],
        "a transaction begun after the commit must see the row"
    );
    db3.execute("ROLLBACK")?;
    Ok(())
}

#[test]
fn a_rolled_back_insert_is_never_visible_to_a_later_transaction() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let mut db1 = Database::open(DbConfig::new(dir.path().join("test.db")))?;
    db1.execute("CREATE TABLE t (a INTEGER)")?;

    db1.execute("BEGIN")?;
    db1.execute("INSERT INTO t VALUES (1)")?;
    db1.execute("ROLLBACK")?;

    assert_eq!(
        rows_of(db1.execute("SELECT * FROM t")?),
        Vec::<Vec<Value>>::new(),
        "a rolled-back insert must never be visible to any later transaction"
    );

    let mut db2 = db1.connect()?;
    assert_eq!(
        rows_of(db2.execute("SELECT * FROM t")?),
        Vec::<Vec<Value>>::new(),
        "a rolled-back insert must not be visible to a different session either"
    );
    Ok(())
}
