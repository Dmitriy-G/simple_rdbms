use std::cell::Cell;
use std::error::Error;
use std::fs::OpenOptions;
use std::path::Path;
use std::rc::Rc;

use common::DbConfig;
use engine::{Database, ResultSet, Tuple};
use storage::block_device::{BlockDevice, DurabilityModel, FaultyDevice, FileDevice};

type DeviceTriple = (Box<dyn BlockDevice>, Box<dyn BlockDevice>, Box<dyn BlockDevice>);

type TableRows = (String, Vec<Tuple>);

fn open_file(path: &Path) -> std::io::Result<std::fs::File> {
    OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path)
}

fn faulty_devices(
    dir: &Path,
    counter: &Rc<Cell<u64>>,
    fail_at: u64,
    model: DurabilityModel,
) -> Result<DeviceTriple, Box<dyn Error>> {
    let db_file = open_file(&dir.join("test.db"))?;
    let wal_file = open_file(&dir.join("test.db.wal"))?;
    let dwb_file = open_file(&dir.join("test.db.dwb"))?;
    let db_device: Box<dyn BlockDevice> = Box::new(FaultyDevice::with_model(
        Box::new(FileDevice::new(db_file)),
        counter.clone(),
        fail_at,
        model,
    ));
    let wal_device: Box<dyn BlockDevice> = Box::new(FaultyDevice::with_model(
        Box::new(FileDevice::new(wal_file)),
        counter.clone(),
        fail_at,
        model,
    ));
    let dwb_device: Box<dyn BlockDevice> = Box::new(FaultyDevice::with_model(
        Box::new(FileDevice::new(dwb_file)),
        counter.clone(),
        fail_at,
        model,
    ));
    Ok((db_device, wal_device, dwb_device))
}

fn config(dir: &Path) -> DbConfig {
    DbConfig::new(dir.join("test.db"))
}

fn total_write_count(workload: &[String]) -> Result<u64, Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let counter = Rc::new(Cell::new(0));
    let (db_device, wal_device, dwb_device) =
        faulty_devices(dir.path(), &counter, u64::MAX, DurabilityModel::write_is_durable())?;
    let mut db =
        Database::open_with_devices(config(dir.path()), db_device, wal_device, dwb_device)?;
    for stmt in workload {
        db.execute(stmt)?;
    }
    db.close()?;
    Ok(counter.get())
}

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

fn run_until_crash(
    config: DbConfig,
    db_device: Box<dyn BlockDevice>,
    wal_device: Box<dyn BlockDevice>,
    dwb_device: Box<dyn BlockDevice>,
    workload: &[String],
) -> Result<usize, Box<dyn Error>> {
    let mut safe_prefix = 0usize;
    if let Ok(mut db) = Database::open_with_devices(config, db_device, wal_device, dwb_device) {
        let mut acked = 0usize;
        let mut in_txn = false;
        for stmt in workload {
            match db.execute(stmt) {
                Ok(_) => {
                    acked += 1;
                    match stmt.as_str() {
                        "BEGIN" => in_txn = true,
                        "COMMIT" | "ROLLBACK" => {
                            in_txn = false;
                            safe_prefix = acked;
                        }
                        _ if !in_txn => safe_prefix = acked,
                        _ => {}
                    }
                }
                Err(_) => break,
            }
        }
    }
    Ok(safe_prefix)
}

fn assert_workload_is_crash_safe(
    workload: &[String],
    model: DurabilityModel,
) -> Result<(), Box<dyn Error>> {
    let k = total_write_count(workload)?;
    assert!(k > 0, "workload must perform at least one write");

    for n in 1..=k {
        let dir = tempfile::tempdir()?;
        let db_path_dir = dir.path();

        let counter = Rc::new(Cell::new(0));
        let (db_device, wal_device, dwb_device) = faulty_devices(db_path_dir, &counter, n, model)?;
        let safe_prefix =
            run_until_crash(config(db_path_dir), db_device, wal_device, dwb_device, workload)?;

        for recovery_fail_at in [1u64, 2] {
            let inner_counter = Rc::new(Cell::new(0));
            let (db_device, wal_device, dwb_device) =
                faulty_devices(db_path_dir, &inner_counter, recovery_fail_at, model)?;
            let _ =
                Database::open_with_devices(config(db_path_dir), db_device, wal_device, dwb_device);
        }

        let mut recovered = Database::open(config(db_path_dir))?;

        let ref_dir = tempfile::tempdir()?;
        let mut reference = Database::open(config(ref_dir.path()))?;
        for stmt in &workload[..safe_prefix] {
            reference.execute(stmt)?;
        }

        let recovered_state = observable_state(&mut recovered)?;
        let reference_state = observable_state(&mut reference)?;
        assert_eq!(
            recovered_state,
            reference_state,
            "model={model:?}, fail_at={n}, safe_prefix={safe_prefix}/{}: recovered state must \
             match replaying exactly the safely-committed prefix",
            workload.len()
        );
    }
    Ok(())
}

fn many_small_inserts() -> Vec<String> {
    let mut stmts = vec!["CREATE TABLE t (a INTEGER)".to_string()];
    for i in 0..12 {
        stmts.push(format!("INSERT INTO t VALUES ({i})"));
    }
    stmts
}

fn page_boundary_inserts() -> Vec<String> {
    let mut stmts = vec!["CREATE TABLE t (a INTEGER, b TEXT)".to_string()];
    let filler = "x".repeat(300);
    for i in 0..20 {
        stmts.push(format!("INSERT INTO t VALUES ({i}, '{filler}')"));
    }
    stmts
}

fn create_table_then_inserts() -> Vec<String> {
    vec![
        "CREATE TABLE solo (a INTEGER)".to_string(),
        "INSERT INTO solo VALUES (1)".to_string(),
        "INSERT INTO solo VALUES (2)".to_string(),
        "INSERT INTO solo VALUES (3)".to_string(),
    ]
}

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

fn mid_transaction_kill() -> Vec<String> {
    vec![
        "CREATE TABLE t (a INTEGER)".to_string(),
        "BEGIN".to_string(),
        "INSERT INTO t VALUES (1)".to_string(),
        "INSERT INTO t VALUES (2)".to_string(),
        "COMMIT".to_string(),
        "BEGIN".to_string(),
        "INSERT INTO t VALUES (3)".to_string(),
    ]
}

fn total_write_count_after(prefix: &[String], workload: &[String]) -> Result<u64, Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    {
        let mut db = Database::open(config(dir.path()))?;
        for stmt in prefix {
            db.execute(stmt)?;
        }
        db.close()?;
    }

    let counter = Rc::new(Cell::new(0));
    let (db_device, wal_device, dwb_device) =
        faulty_devices(dir.path(), &counter, u64::MAX, DurabilityModel::write_is_durable())?;
    let mut db =
        Database::open_with_devices(config(dir.path()), db_device, wal_device, dwb_device)?;
    for stmt in workload {
        db.execute(stmt)?;
    }
    db.close()?;
    Ok(counter.get())
}

fn assert_two_generation_workload_is_crash_safe(
    setup: &[String],
    gen1: &[String],
    gen2: &[String],
    model: DurabilityModel,
) -> Result<(), Box<dyn Error>> {
    let k1 = total_write_count_after(setup, gen1)?;
    assert!(k1 > 0, "generation 1 must perform at least one write");

    for n1 in 1..=k1 {
        let dir = tempfile::tempdir()?;
        let db_path_dir = dir.path();

        {
            let mut db = Database::open(config(db_path_dir))?;
            for stmt in setup {
                db.execute(stmt)?;
            }
            db.close()?;
        }

        let counter = Rc::new(Cell::new(0));
        let (db_device, wal_device, dwb_device) = faulty_devices(db_path_dir, &counter, n1, model)?;
        let safe_prefix1 =
            run_until_crash(config(db_path_dir), db_device, wal_device, dwb_device, gen1)?;

        Database::open(config(db_path_dir))?.close()?;

        {
            let mut db = Database::open(config(db_path_dir))?;
            for stmt in gen2 {
                db.execute(stmt)?;
            }
        }

        let mut recovered = Database::open(config(db_path_dir))?;

        let ref_dir = tempfile::tempdir()?;
        let mut reference = Database::open(config(ref_dir.path()))?;
        for stmt in setup {
            reference.execute(stmt)?;
        }
        for stmt in &gen1[..safe_prefix1] {
            reference.execute(stmt)?;
        }

        let recovered_state = observable_state(&mut recovered)?;
        let reference_state = observable_state(&mut reference)?;
        assert_eq!(
            recovered_state, reference_state,
            "model={model:?}, n1={n1}/{k1}: recovered state after two crash generations must \
             match replaying exactly generation 1's safely-committed prefix (generation 2 never \
             commits)"
        );
    }
    Ok(())
}

fn two_generation_setup() -> Vec<String> {
    vec!["CREATE TABLE t (a INTEGER)".to_string()]
}

fn two_generation_gen1() -> Vec<String> {
    vec!["BEGIN".to_string(), "INSERT INTO t VALUES (1)".to_string(), "COMMIT".to_string()]
}

fn two_generation_gen2() -> Vec<String> {
    vec!["BEGIN".to_string(), "INSERT INTO t VALUES (2)".to_string()]
}

#[test]
fn a_second_generation_transaction_never_corrupts_the_first_at_every_crash_point()
-> Result<(), Box<dyn Error>> {
    assert_two_generation_workload_is_crash_safe(
        &two_generation_setup(),
        &two_generation_gen1(),
        &two_generation_gen2(),
        DurabilityModel::write_is_durable(),
    )
}

#[test]
fn a_second_generation_transaction_never_corrupts_the_first_at_every_crash_point_with_torn_writes()
-> Result<(), Box<dyn Error>> {
    assert_two_generation_workload_is_crash_safe(
        &two_generation_setup(),
        &two_generation_gen1(),
        &two_generation_gen2(),
        DurabilityModel::torn_write(),
    )
}

#[test]
fn a_second_generation_transaction_never_corrupts_the_first_at_every_crash_point_with_unsynced_writes_lost()
-> Result<(), Box<dyn Error>> {
    assert_two_generation_workload_is_crash_safe(
        &two_generation_setup(),
        &two_generation_gen1(),
        &two_generation_gen2(),
        DurabilityModel::requires_sync(),
    )
}

#[test]
fn a_second_generation_transaction_never_corrupts_the_first_at_every_crash_point_with_torn_and_unsynced_writes()
-> Result<(), Box<dyn Error>> {
    assert_two_generation_workload_is_crash_safe(
        &two_generation_setup(),
        &two_generation_gen1(),
        &two_generation_gen2(),
        DurabilityModel::torn_write_requires_sync(),
    )
}

#[test]
fn many_small_inserts_survive_a_crash_at_every_write() -> Result<(), Box<dyn Error>> {
    assert_workload_is_crash_safe(&many_small_inserts(), DurabilityModel::write_is_durable())
}

#[test]
fn many_small_inserts_survive_a_crash_at_every_write_with_unsynced_writes_lost()
-> Result<(), Box<dyn Error>> {
    assert_workload_is_crash_safe(&many_small_inserts(), DurabilityModel::requires_sync())
}

#[test]
fn many_small_inserts_survive_a_crash_at_every_write_with_torn_writes() -> Result<(), Box<dyn Error>>
{
    assert_workload_is_crash_safe(&many_small_inserts(), DurabilityModel::torn_write())
}

#[test]
fn many_small_inserts_survive_a_crash_at_every_write_with_torn_and_unsynced_writes()
-> Result<(), Box<dyn Error>> {
    assert_workload_is_crash_safe(
        &many_small_inserts(),
        DurabilityModel::torn_write_requires_sync(),
    )
}

#[test]
fn page_boundary_inserts_survive_a_crash_at_every_write() -> Result<(), Box<dyn Error>> {
    assert_workload_is_crash_safe(&page_boundary_inserts(), DurabilityModel::write_is_durable())
}

#[test]
fn page_boundary_inserts_survive_a_crash_at_every_write_with_unsynced_writes_lost()
-> Result<(), Box<dyn Error>> {
    assert_workload_is_crash_safe(&page_boundary_inserts(), DurabilityModel::requires_sync())
}

#[test]
fn page_boundary_inserts_survive_a_crash_at_every_write_with_torn_writes()
-> Result<(), Box<dyn Error>> {
    assert_workload_is_crash_safe(&page_boundary_inserts(), DurabilityModel::torn_write())
}

#[test]
fn page_boundary_inserts_survive_a_crash_at_every_write_with_torn_and_unsynced_writes()
-> Result<(), Box<dyn Error>> {
    assert_workload_is_crash_safe(
        &page_boundary_inserts(),
        DurabilityModel::torn_write_requires_sync(),
    )
}

#[test]
fn create_table_then_inserts_survive_a_crash_at_every_write() -> Result<(), Box<dyn Error>> {
    assert_workload_is_crash_safe(&create_table_then_inserts(), DurabilityModel::write_is_durable())
}

#[test]
fn create_table_then_inserts_survive_a_crash_at_every_write_with_unsynced_writes_lost()
-> Result<(), Box<dyn Error>> {
    assert_workload_is_crash_safe(&create_table_then_inserts(), DurabilityModel::requires_sync())
}

#[test]
fn create_table_then_inserts_survive_a_crash_at_every_write_with_torn_writes()
-> Result<(), Box<dyn Error>> {
    assert_workload_is_crash_safe(&create_table_then_inserts(), DurabilityModel::torn_write())
}

#[test]
fn create_table_then_inserts_survive_a_crash_at_every_write_with_torn_and_unsynced_writes()
-> Result<(), Box<dyn Error>> {
    assert_workload_is_crash_safe(
        &create_table_then_inserts(),
        DurabilityModel::torn_write_requires_sync(),
    )
}

#[test]
fn interleaved_allocation_and_catalog_writes_survive_a_crash_at_every_write()
-> Result<(), Box<dyn Error>> {
    assert_workload_is_crash_safe(
        &interleaved_allocation_and_catalog_writes(),
        DurabilityModel::write_is_durable(),
    )
}

#[test]
fn interleaved_allocation_and_catalog_writes_survive_a_crash_at_every_write_with_unsynced_writes_lost()
-> Result<(), Box<dyn Error>> {
    assert_workload_is_crash_safe(
        &interleaved_allocation_and_catalog_writes(),
        DurabilityModel::requires_sync(),
    )
}

#[test]
fn interleaved_allocation_and_catalog_writes_survive_a_crash_at_every_write_with_torn_writes()
-> Result<(), Box<dyn Error>> {
    assert_workload_is_crash_safe(
        &interleaved_allocation_and_catalog_writes(),
        DurabilityModel::torn_write(),
    )
}

#[test]
fn interleaved_allocation_and_catalog_writes_survive_a_crash_at_every_write_with_torn_and_unsynced_writes()
-> Result<(), Box<dyn Error>> {
    assert_workload_is_crash_safe(
        &interleaved_allocation_and_catalog_writes(),
        DurabilityModel::torn_write_requires_sync(),
    )
}

#[test]
fn a_kill_mid_transaction_before_commit_leaves_no_trace_at_every_write()
-> Result<(), Box<dyn Error>> {
    assert_workload_is_crash_safe(&mid_transaction_kill(), DurabilityModel::write_is_durable())
}

#[test]
fn a_kill_mid_transaction_before_commit_leaves_no_trace_at_every_write_with_unsynced_writes_lost()
-> Result<(), Box<dyn Error>> {
    assert_workload_is_crash_safe(&mid_transaction_kill(), DurabilityModel::requires_sync())
}

#[test]
fn a_kill_mid_transaction_before_commit_leaves_no_trace_at_every_write_with_torn_writes()
-> Result<(), Box<dyn Error>> {
    assert_workload_is_crash_safe(&mid_transaction_kill(), DurabilityModel::torn_write())
}

#[test]
fn a_kill_mid_transaction_before_commit_leaves_no_trace_at_every_write_with_torn_and_unsynced_writes()
-> Result<(), Box<dyn Error>> {
    assert_workload_is_crash_safe(
        &mid_transaction_kill(),
        DurabilityModel::torn_write_requires_sync(),
    )
}
