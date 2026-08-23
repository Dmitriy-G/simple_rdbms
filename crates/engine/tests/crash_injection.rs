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
//!
//! ## Durability models
//!
//! Every sweep below runs under every one of the four
//! `storage::block_device::DurabilityModel` compositions:
//!
//! - `write_is_durable()` (the harness's original model): a completed
//!   `write_at`/`set_len` is durable the instant it returns, and the fault
//!   only ever loses the one call it interrupted. This covers a crash that
//!   lands between two syscalls - e.g. the process dying between writing
//!   the WAL and writing a page - which is the failure class M7-M11's fixes
//!   target.
//! - `requires_sync()`: a completed `write_at`/`set_len` only becomes
//!   durable once a later `sync_all` succeeds; anything still pending when
//!   the fault fires is discarded. This additionally covers a crash or
//!   power loss that lands *after* a write() call returns but *before* the
//!   next fsync() - data sitting in the OS page cache, never flushed to the
//!   platter. Real systems lose writes this way constantly;
//!   `write_is_durable()` alone can never exercise it, because nothing
//!   before `requires_sync()` existed for it to be tested by.
//! - `torn_write()`: the tripped `write_at` call itself lands only a
//!   seeded-random subset of its 512-byte sectors on the inner device
//!   before reporting its error - a single `write_at` interrupted
//!   mid-sector, which neither of the models above can produce. This is the
//!   M12 failure class: a checksum (part 1) detects it, and the
//!   double-write buffer (part 2) is what lets recovery repair it instead of
//!   just reporting it. Applied uniformly to all three files (data, WAL,
//!   double-write buffer) like the other models, which also exercises a
//!   torn WAL record (handled by its own per-record CRC, independent of
//!   this file) and a torn double-write buffer slot or header (handled by
//!   `recovery::recover_double_write` treating a bad copy as "skip it,"
//!   not just a torn real-file page).
//! - `torn_write_requires_sync()`: `torn_write()` and `requires_sync()`
//!   composed - the tripped call tears *and* everything else still needs a
//!   `sync_all` to be durable, so whatever was pending when the fault fired
//!   is lost outright rather than merely absent from that one call. This is
//!   what an actual power failure does, and it is the combination the
//!   double-write buffer exists to survive: the other three models each
//!   leave one of tearing or lost-unsynced-writes out, so none of them on
//!   its own tests the double-write buffer against the failure mode it was
//!   built for.
//!
//! None of the four models cover a disk that lies about having synced (some
//! consumer SSDs and virtualized/cloud block devices under certain
//! configurations), or media failure large enough to fool CRC-32 in both a
//! page and its double-write copy at once. Those are outside what fault
//! injection at the `write_at`/`sync_all` call boundary can express at all.

use std::cell::Cell;
use std::error::Error;
use std::fs::OpenOptions;
use std::path::Path;
use std::rc::Rc;

use common::DbConfig;
use engine::{Database, ResultSet, Tuple};
use storage::block_device::{BlockDevice, DurabilityModel, FaultyDevice, FileDevice};

/// A `Database::open_with_devices` call's trio of devices: the database
/// file, the write-ahead log, and the double-write buffer.
type DeviceTriple = (Box<dyn BlockDevice>, Box<dyn BlockDevice>, Box<dyn BlockDevice>);

/// One table's name alongside every row in it, in the order produced.
type TableRows = (String, Vec<Tuple>);

/// Opens (creating if necessary) the file at `path` for a `FileDevice`.
fn open_file(path: &Path) -> std::io::Result<std::fs::File> {
    OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path)
}

/// Builds the trio of `BlockDevice`s a `Database::open_with_devices` call
/// needs, all sharing `counter` and armed to fail at write `fail_at` under
/// `model`, so that "fail at write N" counts across the whole system rather
/// than per file - matching how a real crash lands mid-workload regardless
/// of which file the Nth write happened to target.
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

/// Runs `workload` to completion (including a clean `close`) against a
/// counting-but-never-failing device pair, returning the total number of
/// writes performed - the upper bound `K` for the crash-injection sweep.
/// The call count is identical under either `DurabilityModel` (the fault
/// counter ticks on every `write_at`/`set_len` call regardless of whether
/// that call lands immediately or waits for a `sync_all`), so which model
/// this runs under doesn't affect `K`.
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

/// Runs `workload` against a freshly opened database backed by
/// `db_device`/`wal_device` (typically fault-injecting ones) until a
/// statement fails or the workload finishes, returning how large a prefix
/// of it is safe to expect on the other side of a crash right afterward.
///
/// `acked` (every statement that returned `Ok`) is not the same as "safely
/// committed": a `BEGIN` and the inserts after it can each individually
/// return `Ok` while the fault then kills the process before the matching
/// `COMMIT` ever runs, in which case none of them should be in the
/// reference prefix. The returned count only advances at a point nothing is
/// left open - after an autocommit statement, or after a `COMMIT`/
/// `ROLLBACK` that actually returned `Ok`.
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
        // `db` is dropped here without `close`: it died wherever it died,
        // exactly like a real crash.
    }
    Ok(safe_prefix)
}

/// Runs the full crash-injection sweep for `workload` under `model`: every
/// fail point `1..=K`, plus (per fail point) a couple of secondary
/// fault-injected reopens that crash *during* recovery itself before the
/// final clean reopen - proving recovery resumes correctly rather than
/// restarting.
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

        // A couple of secondary crashes *during* the next recovery attempt
        // itself, on the very same files, before the final clean reopen.
        // Each must not panic (an `Err` is fine - it's still a crash); what
        // matters is that recovery is resumable, not that any one attempt
        // succeeds.
        for recovery_fail_at in [1u64, 2] {
            let inner_counter = Rc::new(Cell::new(0));
            let (db_device, wal_device, dwb_device) =
                faulty_devices(db_path_dir, &inner_counter, recovery_fail_at, model)?;
            let _ =
                Database::open_with_devices(config(db_path_dir), db_device, wal_device, dwb_device);
        }

        // Reopen for real: recovery must leave a fully consistent database
        // behind regardless of how many faulty attempts preceded it.
        let mut recovered = Database::open(config(db_path_dir))?;

        // The reference: replay exactly the acknowledged prefix against a
        // fault-free database.
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

/// A committed explicit transaction followed by one that's deliberately
/// left open (its own trailing `BEGIN`/`INSERT` never reach a `COMMIT`
/// within the fixed workload). Every fail point at or after that trailing
/// `BEGIN` must recover to "row 3 doesn't exist" - this is the M8
/// "kill mid-transaction, before COMMIT, leaves no trace of it" case, swept
/// across every write point rather than checked at just one.
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

/// Like `total_write_count`, but for a workload meant to run as a second
/// process generation on top of `prefix` having already run to completion
/// and been cleanly closed: returns the write count for `workload` alone,
/// with `prefix`'s own writes (and that clean close's) excluded.
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

/// Crosses two process generations, the one shape none of the sweeps above
/// exercise: every prior sweep crashes once and then only ever reopens
/// cleanly afterward, so a freshly opened `TransactionManager` never gets a
/// chance to collide with an id a *previous* generation's crash left
/// dangling - which is exactly the M9 bug (see `task.MD`) this harness
/// extension targets: a transaction whose `Commit` reached disk but whose
/// `End` didn't is a "phantom winner" that recovery correctly never undoes,
/// but if a later generation's `TransactionManager` isn't seeded past its
/// id, a brand new transaction can be assigned that same id - and, unless
/// recovery resets that id's Active Transaction Table state on `Begin`,
/// the new, uncommitted transaction inherits the phantom's `committed`
/// flag and is never undone either.
///
/// `setup` runs once, cleanly, before the sweep even starts, so both
/// generations' own first explicit transaction lands on the same id under
/// the pre-fix bug (every open's bootstrap transaction takes id `0`, and
/// each generation's own first explicit `BEGIN` is otherwise the very next
/// id handed out) - the collision this test needs actually happens, not
/// just hypothetically could. `gen1` is swept across every possible crash
/// point (`1..=K1`), since only some of them land exactly between its
/// `COMMIT`'s flush and its `End` ever reaching disk - the precise
/// "phantom winner" shape. `gen2` is not fault-injected: it is constructed
/// to never issue its own `COMMIT`, so simply running it to completion and
/// dropping the database without closing already leaves it open, exactly
/// like a real process exit mid-transaction.
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

        // Setup: applied cleanly, outside fault injection, so it never
        // counts against `n1`'s budget.
        {
            let mut db = Database::open(config(db_path_dir))?;
            for stmt in setup {
                db.execute(stmt)?;
            }
            db.close()?;
        }

        // Generation 1: runs under fault injection until it crashes.
        let counter = Rc::new(Cell::new(0));
        let (db_device, wal_device, dwb_device) = faulty_devices(db_path_dir, &counter, n1, model)?;
        let safe_prefix1 =
            run_until_crash(config(db_path_dir), db_device, wal_device, dwb_device, gen1)?;

        // Reopen for real: recovery must leave a fully consistent database
        // behind. Closing it cleanly means generation 2 below starts from a
        // clean slate rather than layering a second crash on top of the
        // first.
        Database::open(config(db_path_dir))?.close()?;

        // Generation 2: never commits by construction, so running it to
        // completion and then just dropping the database (no `close`)
        // already leaves it exactly as open as a real crash mid-transaction
        // would.
        {
            let mut db = Database::open(config(db_path_dir))?;
            for stmt in gen2 {
                db.execute(stmt)?;
            }
        }

        // The final reopen: this is where a transaction id reused across
        // generations would corrupt generation 1's own already-committed
        // data, if `TransactionManager` weren't seeded past every id the
        // log has ever used.
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

/// The table both generations act on, created once via a clean, fault-free
/// open before the sweep starts.
fn two_generation_setup() -> Vec<String> {
    vec!["CREATE TABLE t (a INTEGER)".to_string()]
}

/// Generation 1: a single explicit transaction committing one row, swept
/// across every crash point - including, crucially, every point after its
/// `COMMIT` record is durable but before its `End` is.
fn two_generation_gen1() -> Vec<String> {
    vec!["BEGIN".to_string(), "INSERT INTO t VALUES (1)".to_string(), "COMMIT".to_string()]
}

/// Generation 2: a second explicit transaction that never commits.
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

/// Same as above, but under `DurabilityModel::torn_write()`: a torn write to
/// the real data file mid-flush must be repaired by the double-write buffer
/// before recovery ever gets to the transaction-id-reuse hazard this sweep
/// exists to catch, not compound it.
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

/// Same shape again, but this workload had never been swept under
/// `requires_sync()` at all - so add both the missing model and the
/// missing composition of it with tearing, completing the set of all four.
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

/// The composed model: the transaction-id-reuse hazard must still be caught
/// even when the crash that exposes it also tears a write and discards
/// whatever was unsynced.
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
