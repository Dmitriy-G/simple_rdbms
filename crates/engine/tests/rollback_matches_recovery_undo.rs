use std::error::Error;

use common::DbConfig;
use engine::Database;

fn config(dir: &std::path::Path) -> DbConfig {
    DbConfig::new(dir.join("test.db"))
}

#[test]
fn rollback_and_recovery_undo_produce_byte_identical_files() -> Result<(), Box<dyn Error>> {
    let workload_without_rollback = [
        "CREATE TABLE t (a INTEGER)",
        "BEGIN",
        "INSERT INTO t VALUES (1)",
        "INSERT INTO t VALUES (2)",
    ];

    let dir_a = tempfile::tempdir()?;
    let path_a = dir_a.path().join("test.db");
    {
        let mut db = Database::open(config(dir_a.path()))?;
        for stmt in workload_without_rollback {
            db.execute(stmt)?;
        }
        db.execute("ROLLBACK")?;
    }
    let bytes_a = std::fs::read(&path_a)?;

    let dir_b = tempfile::tempdir()?;
    let path_b = dir_b.path().join("test.db");
    {
        let mut db = Database::open(config(dir_b.path()))?;
        for stmt in workload_without_rollback {
            db.execute(stmt)?;
        }
    }
    {
        let db = Database::open(config(dir_b.path()))?;
        drop(db);
    }
    let bytes_b = std::fs::read(&path_b)?;

    assert_eq!(
        bytes_a, bytes_b,
        "rollback and recovery-undo of the same aborted transaction must leave byte-identical \
         database files - proof that ROLLBACK and crash recovery share one undo implementation"
    );
    Ok(())
}
