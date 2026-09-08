use std::error::Error;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use common::DbConfig;
use engine::Database;
use types::Value;

mod support;
use support::rows_of;

const BLOCKED_WINDOW: Duration = Duration::from_millis(300);
const COMPLETION_TIMEOUT: Duration = Duration::from_secs(10);
const SEED_ROWS: i32 = 200;
const CONCURRENT_ROWS: i32 = 200;

fn sorted_ints(rows: Vec<Vec<Value>>) -> Vec<i32> {
    let mut values: Vec<i32> = rows
        .into_iter()
        .map(|row| match row.first() {
            Some(Value::Integer(n)) => *n,
            other => panic!("expected a single INTEGER column, got {other:?}"),
        })
        .collect();
    values.sort_unstable();
    values
}

#[test]
fn create_index_waits_for_an_uncommitted_insert_and_never_indexes_it() -> Result<(), Box<dyn Error>>
{
    let dir = tempfile::tempdir()?;
    let db = Database::open(DbConfig::new(dir.path().join("test.db")))?;

    let mut reader = db.connect()?;
    reader.execute("CREATE TABLE t (a INTEGER)")?;
    reader.execute("INSERT INTO t VALUES (1), (2), (3)")?;

    let mut writer = db.connect()?;
    writer.execute("BEGIN")?;
    writer.execute("INSERT INTO t VALUES (99)")?;

    let mut builder = db.connect()?;
    let (done_tx, done_rx) = mpsc::channel();
    let build = thread::spawn(move || {
        let result =
            builder.execute("CREATE INDEX idx_t_a ON t (a)").map_err(|err| err.to_string());
        let _ = done_tx.send(());
        result
    });

    assert!(
        done_rx.recv_timeout(BLOCKED_WINDOW).is_err(),
        "CREATE INDEX must block behind the open transaction's exclusive table lock rather than \
         backfilling an uncommitted row"
    );

    writer.execute("ROLLBACK")?;
    done_rx
        .recv_timeout(COMPLETION_TIMEOUT)
        .expect("CREATE INDEX must finish once the writer releases its table lock");
    build.join().expect("the CREATE INDEX thread must not panic")?;

    let dirty = rows_of(reader.execute("SELECT a FROM t WHERE a = 99")?);
    assert!(
        dirty.is_empty(),
        "the rolled-back row must never have reached the index; got {dirty:?}"
    );
    let committed = rows_of(reader.execute("SELECT a FROM t WHERE a = 2")?);
    assert_eq!(committed, vec![vec![Value::Integer(2)]]);
    Ok(())
}

#[test]
fn an_index_built_beside_a_concurrent_insert_holds_every_committed_row()
-> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let db = Database::open(DbConfig::new(dir.path().join("test.db")))?;

    let mut reader = db.connect()?;
    reader.execute("CREATE TABLE t (a INTEGER)")?;
    for n in 0..SEED_ROWS {
        reader.execute(&format!("INSERT INTO t VALUES ({n})"))?;
    }

    let mut inserter = db.connect()?;
    let insert_handle = thread::spawn(move || -> Result<(), String> {
        for n in SEED_ROWS..SEED_ROWS + CONCURRENT_ROWS {
            inserter
                .execute(&format!("INSERT INTO t VALUES ({n})"))
                .map_err(|err| err.to_string())?;
        }
        Ok(())
    });

    let mut builder = db.connect()?;
    let build_handle = thread::spawn(move || -> Result<(), String> {
        builder.execute("CREATE INDEX idx_t_a ON t (a)").map(|_| ()).map_err(|err| err.to_string())
    });

    insert_handle.join().expect("the INSERT thread must not panic")?;
    build_handle.join().expect("the CREATE INDEX thread must not panic")?;

    let scanned = sorted_ints(rows_of(reader.execute("SELECT a FROM t")?));
    let indexed = sorted_ints(rows_of(reader.execute("SELECT a FROM t WHERE a >= 0")?));
    assert_eq!(
        scanned.len(),
        (SEED_ROWS + CONCURRENT_ROWS) as usize,
        "every committed row must be in the heap"
    );
    assert_eq!(
        indexed, scanned,
        "an index built next to a concurrent INSERT must return exactly what the sequential scan \
         returns"
    );
    Ok(())
}
