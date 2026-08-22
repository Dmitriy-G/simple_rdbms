//! Proves `TransactionManager::abort` (what `ROLLBACK` calls) and
//! `storage::recovery::recover`'s own Undo pass really are one
//! implementation, not two that happen to agree on paper: undoing the same
//! transaction's writes via an explicit `ROLLBACK` and via a crash before
//! that `ROLLBACK` ever runs must leave byte-identical database files.
//!
//! Neither scenario calls `close` (which would write a checkpoint, adding
//! bytes unrelated to undo itself and unique to whichever scenario
//! happened to call it) - each is just dropped, the same "no shutdown code
//! ran" starting point a real crash leaves behind. No fault injection is
//! needed here: scenario B's "crash" is simply never issuing the
//! `ROLLBACK` statement and dropping the database, which is exactly what a
//! kill signal would leave behind - `Database::open`'s recovery pass is
//! then what has to notice the still-active transaction and undo it.

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

    // Scenario A: the transaction is rolled back explicitly, in-process.
    let dir_a = tempfile::tempdir()?;
    let path_a = dir_a.path().join("test.db");
    {
        let mut db = Database::open(config(dir_a.path()))?;
        for stmt in workload_without_rollback {
            db.execute(stmt)?;
        }
        db.execute("ROLLBACK")?;
        // Dropped without `close`.
    }
    let bytes_a = std::fs::read(&path_a)?;

    // Scenario B: the same transaction never reaches `ROLLBACK` - the
    // process dies right after its last write, and recovery's Undo pass
    // (the exact same `storage::recovery::undo_transaction` `abort` calls)
    // cleans it up on the next open instead.
    let dir_b = tempfile::tempdir()?;
    let path_b = dir_b.path().join("test.db");
    {
        let mut db = Database::open(config(dir_b.path()))?;
        for stmt in workload_without_rollback {
            db.execute(stmt)?;
        }
        // Dropped without `close` and without `ROLLBACK`: simulates a kill
        // signal landing right here, mid-transaction.
    }
    {
        let db = Database::open(config(dir_b.path()))?; // runs recovery's Undo pass
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
