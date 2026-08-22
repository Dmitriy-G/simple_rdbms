//! The crash-injection harness: for a fixed, deterministic workload of SQL
//! statements, and for every possible point a write could fail (`1..=K`,
//! where `K` is the total number of writes the workload performs), opens a
//! fresh database on a `FaultyDevice` armed to fail at write `n`, runs the
//! workload until it dies, reopens the same files on a real device (which
//! runs `storage::recovery::recover` as part of `Database::open`), and
//! checks the result is consistent with *some* prefix of the workload's
//! statements: build a reference database by replaying exactly the
//! statements that were acknowledged before the fault struck (against a
//! fault-free device), and require the recovered database's observable
//! state - its set of tables and every row in each - to match it exactly.
//!
//! This is the M7 deliverable per `task.MD`: "this harness will find more
//! bugs than every other test in the repo combined."

use std::cell::Cell;
use std::error::Error;
use std::fs::OpenOptions;
use std::path::Path;
use std::rc::Rc;

use common::DbConfig;
use engine::{Database, ResultSet, Tuple};
use storage::block_device::{BlockDevice, FaultyDevice, FileDevice};

/// A `Database::open_with_devices` call's pair of devices: one for the
/// database file, one for the write-ahead log.
type DevicePair = (Box<dyn BlockDevice>, Box<dyn BlockDevice>);

/// One table's name alongside every row in it, in the order produced.
type TableRows = (String, Vec<Tuple>);

/// Opens (creating if necessary) the file at `path` for a `FileDevice`.
fn open_file(path: &Path) -> std::io::Result<std::fs::File> {
    OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path)
}

/// Builds the pair of `BlockDevice`s a `Database::open_with_devices` call
/// needs, both sharing `counter` and armed to fail at write `fail_at`, so
/// that "fail at write N" counts across the whole system rather than per
/// file - matching how a real crash lands mid-workload regardless of which
/// file the Nth write happened to target.
fn faulty_devices(
    dir: &Path,
    counter: &Rc<Cell<u64>>,
    fail_at: u64,
) -> Result<DevicePair, Box<dyn Error>> {
    let db_file = open_file(&dir.join("test.db"))?;
    let wal_file = open_file(&dir.join("test.db.wal"))?;
    let db_device: Box<dyn BlockDevice> =
        Box::new(FaultyDevice::new(Box::new(FileDevice::new(db_file)), counter.clone(), fail_at));
    let wal_device: Box<dyn BlockDevice> =
        Box::new(FaultyDevice::new(Box::new(FileDevice::new(wal_file)), counter.clone(), fail_at));
    Ok((db_device, wal_device))
}

fn config(dir: &Path) -> DbConfig {
    DbConfig::new(dir.join("test.db"))
}

/// Runs `workload` to completion (including a clean `close`) against a
/// counting-but-never-failing device pair, returning the total number of
/// writes performed - the upper bound `K` for the crash-injection sweep.
fn total_write_count(workload: &[String]) -> Result<u64, Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let counter = Rc::new(Cell::new(0));
    let (db_device, wal_device) = faulty_devices(dir.path(), &counter, u64::MAX)?;
    let mut db = Database::open_with_devices(config(dir.path()), db_device, wal_device)?;
    for stmt in workload {
        db.execute(stmt)?;
    }
    db.close()?;
    Ok(counter.get())
}

/// Every table's name (sorted) and every row in each, in the order
/// produced - the "observable state" two databases are compared on.
fn observable_state(db: &mut Database) -> Result<Vec<TableRows>, Box<dyn Error>> {
    let mut tables = db.table_names();
    tables.sort();
    tables
        .into_iter()
        .map(|table| {
            let ResultSet::Rows { rows, .. } = db.execute(&format!("SELECT * FROM {table}"))?
            else {
                return Err("expected a Rows result set from SELECT".into());
            };
            Ok((table, rows))
        })
        .collect()
}

/// Runs the full crash-injection sweep for `workload`: every fail point
/// `1..=K`, plus (per fail point) a couple of secondary fault-injected
/// reopens that crash *during* recovery itself before the final clean
/// reopen - proving recovery resumes correctly rather than restarting.
fn assert_workload_is_crash_safe(workload: &[String]) -> Result<(), Box<dyn Error>> {
    let k = total_write_count(workload)?;
    assert!(k > 0, "workload must perform at least one write");

    for n in 1..=k {
        let dir = tempfile::tempdir()?;
        let db_path_dir = dir.path();

        let counter = Rc::new(Cell::new(0));
        let (db_device, wal_device) = faulty_devices(db_path_dir, &counter, n)?;

        let mut acked = 0usize;
        if let Ok(mut db) = Database::open_with_devices(config(db_path_dir), db_device, wal_device)
        {
            for stmt in workload {
                match db.execute(stmt) {
                    Ok(_) => acked += 1,
                    Err(_) => break,
                }
            }
            // `db` is dropped here without `close`: it died wherever it
            // died, exactly like a real crash.
        }

        // A couple of secondary crashes *during* the next recovery attempt
        // itself, on the very same files, before the final clean reopen.
        // Each must not panic (an `Err` is fine - it's still a crash); what
        // matters is that recovery is resumable, not that any one attempt
        // succeeds.
        for recovery_fail_at in [1u64, 2] {
            let inner_counter = Rc::new(Cell::new(0));
            let (db_device, wal_device) =
                faulty_devices(db_path_dir, &inner_counter, recovery_fail_at)?;
            let _ = Database::open_with_devices(config(db_path_dir), db_device, wal_device);
        }

        // Reopen for real: recovery must leave a fully consistent database
        // behind regardless of how many faulty attempts preceded it.
        let mut recovered = Database::open(config(db_path_dir))?;

        // The reference: replay exactly the acknowledged prefix against a
        // fault-free database.
        let ref_dir = tempfile::tempdir()?;
        let mut reference = Database::open(config(ref_dir.path()))?;
        for stmt in &workload[..acked] {
            reference.execute(stmt)?;
        }

        let recovered_state = observable_state(&mut recovered)?;
        let reference_state = observable_state(&mut reference)?;
        assert_eq!(
            recovered_state,
            reference_state,
            "fail_at={n}, acked={acked}/{}: recovered state must match replaying exactly the \
             acknowledged prefix",
            workload.len()
        );
    }
    Ok(())
}

/// Many tiny single-column inserts into one table.
fn many_small_inserts() -> Vec<String> {
    let mut stmts = vec!["CREATE TABLE t (a INTEGER)".to_string()];
    for i in 0..12 {
        stmts.push(format!("INSERT INTO t VALUES ({i})"));
    }
    stmts
}

/// Wide rows, enough of them to force at least one heap page allocation
/// mid-workload (exercising `AllocPage`'s redo alongside `Update`'s).
fn page_boundary_inserts() -> Vec<String> {
    let mut stmts = vec!["CREATE TABLE t (a INTEGER, b TEXT)".to_string()];
    let filler = "x".repeat(300);
    for i in 0..20 {
        stmts.push(format!("INSERT INTO t VALUES ({i}, '{filler}')"));
    }
    stmts
}

/// A `CREATE TABLE` immediately followed by a handful of inserts - short
/// enough that a large fraction of the crash-injection sweep's fail points
/// land inside the `CREATE TABLE`'s own catalog-heap write.
fn create_table_then_inserts() -> Vec<String> {
    vec![
        "CREATE TABLE solo (a INTEGER)".to_string(),
        "INSERT INTO solo VALUES (1)".to_string(),
        "INSERT INTO solo VALUES (2)".to_string(),
        "INSERT INTO solo VALUES (3)".to_string(),
    ]
}

/// Two tables' `CREATE TABLE`s (catalog writes) and inserts (heap page
/// allocation) interleaved, rather than one table fully built before the
/// next starts.
fn interleaved_allocation_and_catalog_writes() -> Vec<String> {
    vec![
        "CREATE TABLE a (x INTEGER)".to_string(),
        "INSERT INTO a VALUES (1)".to_string(),
        "CREATE TABLE b (y INTEGER)".to_string(),
        "INSERT INTO b VALUES (10)".to_string(),
        "INSERT INTO a VALUES (2)".to_string(),
        "INSERT INTO b VALUES (20)".to_string(),
        "INSERT INTO a VALUES (3)".to_string(),
    ]
}

#[test]
fn many_small_inserts_survive_a_crash_at_every_write() -> Result<(), Box<dyn Error>> {
    assert_workload_is_crash_safe(&many_small_inserts())
}

#[test]
fn page_boundary_inserts_survive_a_crash_at_every_write() -> Result<(), Box<dyn Error>> {
    assert_workload_is_crash_safe(&page_boundary_inserts())
}

#[test]
fn create_table_then_inserts_survive_a_crash_at_every_write() -> Result<(), Box<dyn Error>> {
    assert_workload_is_crash_safe(&create_table_then_inserts())
}

#[test]
fn interleaved_allocation_and_catalog_writes_survive_a_crash_at_every_write()
-> Result<(), Box<dyn Error>> {
    assert_workload_is_crash_safe(&interleaved_allocation_and_catalog_writes())
}
