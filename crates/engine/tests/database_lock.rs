use std::error::Error;
use std::path::PathBuf;

use common::{DbConfig, Error as CommonError, SqlState};
use engine::Database;
use storage::wal::{FileSegmentStore, SegmentStore, segment_path};

#[test]
fn a_second_database_open_reports_a_usable_error() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let config = DbConfig::new(dir.path().join("locked.db"));

    let _first = Database::open(config.clone())?;

    match Database::open(config) {
        Err(err @ CommonError::DatabaseLocked { .. }) => {
            assert_eq!(err.sql_state(), SqlState::LOCK_NOT_AVAILABLE);
        }
        Err(other) => panic!("expected DatabaseLocked, got {other}"),
        Ok(_) => panic!("a second open of a locked database must be refused"),
    }
    Ok(())
}

type SegmentSnapshot = Vec<(PathBuf, Vec<u8>)>;

fn segment_bytes(wal_base: &std::path::Path) -> Result<SegmentSnapshot, Box<dyn Error>> {
    let store = FileSegmentStore::new(wal_base);
    store
        .existing_segments()?
        .into_iter()
        .map(|id| {
            let path = segment_path(wal_base, id);
            let bytes = std::fs::read(&path)?;
            Ok((path, bytes))
        })
        .collect()
}

#[test]
fn a_refused_second_open_does_not_touch_the_wal() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let config = DbConfig::new(dir.path().join("locked.db"));
    let mut wal_base = config.db_path.clone().into_os_string();
    wal_base.push(".wal");
    let wal_base = PathBuf::from(wal_base);

    let mut first = Database::open(config.clone())?;
    first.execute("CREATE TABLE t (a INTEGER)")?;
    first.execute("INSERT INTO t VALUES (1)")?;

    let active_id = *FileSegmentStore::new(&wal_base)
        .existing_segments()?
        .last()
        .expect("the first database must have an active WAL segment");
    let active_path = segment_path(&wal_base, active_id);
    {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new().append(true).open(&active_path)?;
        file.write_all(b"torn-tail-bytes-past-the-last-valid-record")?;
    }

    let before = segment_bytes(&wal_base)?;
    assert!(!before.is_empty(), "the first database must have written at least one WAL segment");

    match Database::open(config) {
        Err(CommonError::DatabaseLocked { .. }) => {}
        Err(other) => panic!("expected DatabaseLocked, got {other}"),
        Ok(_) => panic!("a second open of a locked database must be refused"),
    }

    let after = segment_bytes(&wal_base)?;
    assert_eq!(
        before, after,
        "a refused second open must not have written, truncated, or rolled any WAL segment"
    );

    drop(first);
    Ok(())
}

#[test]
fn a_database_can_be_reopened_immediately_after_the_previous_one_drops()
-> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    for i in 0..50 {
        let mut db = Database::open(DbConfig::new(dir.path().join("db")))?;
        db.execute(&format!("CREATE TABLE t{i} (a INTEGER)"))?;
        db.close()?;
        let db = Database::open(DbConfig::new(dir.path().join("db")))?;
        drop(db);
    }
    Ok(())
}
