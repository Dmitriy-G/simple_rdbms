use std::error::Error;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use common::DbConfig;
use engine::{Database, ResultSet, Tuple};
use storage::block_device::DurabilityModel;
use storage::wal::{DEFAULT_SEGMENT_SIZE, FileSegmentStore, SegmentStore};
use test_support::{CrashWorkload, DeviceTriple, assert_workload_is_crash_safe, faulty_devices};

type TableRows = (String, Vec<Tuple>);

#[derive(Clone, Copy)]
struct HarnessConfig {
    wal_segment_size: u64,
    checkpoint_byte_threshold: u64,
    buffer_pool_size: usize,
}

impl HarnessConfig {
    const DEFAULT: Self = Self {
        wal_segment_size: DEFAULT_SEGMENT_SIZE,
        checkpoint_byte_threshold: DbConfig::DEFAULT_CHECKPOINT_BYTE_THRESHOLD,
        buffer_pool_size: DbConfig::DEFAULT_BUFFER_POOL_SIZE,
    };

    const SEGMENT_ROLLING: Self =
        Self { wal_segment_size: 4096, checkpoint_byte_threshold: 1024, buffer_pool_size: 8 };
}

fn config(dir: &Path, harness: HarnessConfig) -> DbConfig {
    DbConfig {
        checkpoint_byte_threshold: harness.checkpoint_byte_threshold,
        buffer_pool_size: harness.buffer_pool_size,
        ..test_support::db_config(dir)
    }
}

fn wal_base_path(dir: &Path) -> std::path::PathBuf {
    let mut path = dir.join("test.db").into_os_string();
    path.push(".wal");
    std::path::PathBuf::from(path)
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

struct SqlWorkload<'a> {
    statements: &'a [String],
    harness: HarnessConfig,
}

impl CrashWorkload for SqlWorkload<'_> {
    type Handle = Database;
    type State = Vec<TableRows>;

    fn item_count(&self) -> usize {
        self.statements.len()
    }

    fn open(&self, dir: &Path, devices: DeviceTriple) -> Result<Self::Handle, Box<dyn Error>> {
        let (db_device, wal_store, dwb_device) = devices;
        Ok(Database::open_with_devices(
            config(dir, self.harness),
            db_device,
            wal_store,
            self.harness.wal_segment_size,
            dwb_device,
        )?)
    }

    fn drive(&self, db: &mut Self::Handle) -> usize {
        let mut acked = 0usize;
        let mut in_txn = false;
        let mut safe_prefix = 0usize;
        for stmt in self.statements {
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
        safe_prefix
    }

    fn expected_state(&self, safe_prefix: usize) -> Result<Self::State, Box<dyn Error>> {
        let ref_dir = tempfile::tempdir()?;
        let mut reference = Database::open(config(ref_dir.path(), self.harness))?;
        for stmt in &self.statements[..safe_prefix] {
            reference.execute(stmt)?;
        }
        observable_state(&mut reference)
    }

    fn observed_state(
        &self,
        _safe_prefix: usize,
        db: &mut Self::Handle,
    ) -> Result<Self::State, Box<dyn Error>> {
        observable_state(db)
    }
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

fn segment_rolling_inserts() -> Vec<String> {
    let mut stmts = vec!["CREATE TABLE t (a INTEGER, b TEXT)".to_string()];
    let filler = "x".repeat(1800);
    for i in 0..20 {
        stmts.push(format!("INSERT INTO t VALUES ({i}, '{filler}')"));
    }
    stmts
}

fn assert_segment_rolling_and_truncation_occurred(
    workload: &[String],
    harness: HarnessConfig,
) -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let counter = Arc::new(AtomicU64::new(0));
    let (db_device, wal_store, dwb_device) =
        faulty_devices(dir.path(), &counter, u64::MAX, DurabilityModel::write_is_durable())?;
    let mut db = Database::open_with_devices(
        config(dir.path(), harness),
        db_device,
        wal_store,
        harness.wal_segment_size,
        dwb_device,
    )?;
    for stmt in workload {
        db.execute(stmt)?;
    }
    db.close()?;

    let store = FileSegmentStore::new(wal_base_path(dir.path()));
    let ids = store.existing_segments()?;
    assert!(
        ids.len() >= 2,
        "segment_rolling_inserts must actually roll past the first segment at a {}-byte segment \
         size, got {} segment(s) on disk",
        harness.wal_segment_size,
        ids.len()
    );
    assert!(
        ids[0] > 0,
        "segment_rolling_inserts must trigger at least one checkpoint-driven truncation of \
         segment 0, but segment 0 still exists: {ids:?}"
    );
    Ok(())
}

fn total_write_count_after(
    prefix: &[String],
    workload: &[String],
    harness: HarnessConfig,
) -> Result<u64, Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    {
        let mut db = Database::open(config(dir.path(), harness))?;
        for stmt in prefix {
            db.execute(stmt)?;
        }
        db.close()?;
    }

    let counter = Arc::new(AtomicU64::new(0));
    let (db_device, wal_store, dwb_device) =
        faulty_devices(dir.path(), &counter, u64::MAX, DurabilityModel::write_is_durable())?;
    let mut db = Database::open_with_devices(
        config(dir.path(), harness),
        db_device,
        wal_store,
        harness.wal_segment_size,
        dwb_device,
    )?;
    for stmt in workload {
        db.execute(stmt)?;
    }
    db.close()?;
    Ok(counter.load(Ordering::Relaxed))
}

fn assert_two_generation_workload_is_crash_safe(
    setup: &[String],
    gen1: &[String],
    gen2: &[String],
    model: DurabilityModel,
    harness: HarnessConfig,
) -> Result<(), Box<dyn Error>> {
    let k1 = total_write_count_after(setup, gen1, harness)?;
    assert!(k1 > 0, "generation 1 must perform at least one write");

    let gen1_workload = SqlWorkload { statements: gen1, harness };

    for n1 in 1..=k1 {
        let dir = tempfile::tempdir()?;
        let db_path_dir = dir.path();

        {
            let mut db = Database::open(config(db_path_dir, harness))?;
            for stmt in setup {
                db.execute(stmt)?;
            }
            db.close()?;
        }

        let counter = Arc::new(AtomicU64::new(0));
        let devices = faulty_devices(db_path_dir, &counter, n1, model)?;
        let safe_prefix1 = match gen1_workload.open(db_path_dir, devices) {
            Ok(mut db) => gen1_workload.drive(&mut db),
            Err(_) => 0,
        };

        Database::open(config(db_path_dir, harness))?.close()?;

        {
            let mut db = Database::open(config(db_path_dir, harness))?;
            for stmt in gen2 {
                db.execute(stmt)?;
            }
        }

        let mut recovered = Database::open(config(db_path_dir, harness))?;

        let ref_dir = tempfile::tempdir()?;
        let mut reference = Database::open(config(ref_dir.path(), harness))?;
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
        HarnessConfig::DEFAULT,
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
        HarnessConfig::DEFAULT,
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
        HarnessConfig::DEFAULT,
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
        HarnessConfig::DEFAULT,
    )
}

#[test]
fn many_small_inserts_survive_a_crash_at_every_write() -> Result<(), Box<dyn Error>> {
    let statements = many_small_inserts();
    assert_workload_is_crash_safe(
        &SqlWorkload { statements: &statements, harness: HarnessConfig::DEFAULT },
        DurabilityModel::write_is_durable(),
    )
}

#[test]
fn many_small_inserts_survive_a_crash_at_every_write_with_unsynced_writes_lost()
-> Result<(), Box<dyn Error>> {
    let statements = many_small_inserts();
    assert_workload_is_crash_safe(
        &SqlWorkload { statements: &statements, harness: HarnessConfig::DEFAULT },
        DurabilityModel::requires_sync(),
    )
}

#[test]
fn many_small_inserts_survive_a_crash_at_every_write_with_torn_writes() -> Result<(), Box<dyn Error>>
{
    let statements = many_small_inserts();
    assert_workload_is_crash_safe(
        &SqlWorkload { statements: &statements, harness: HarnessConfig::DEFAULT },
        DurabilityModel::torn_write(),
    )
}

#[test]
fn many_small_inserts_survive_a_crash_at_every_write_with_torn_and_unsynced_writes()
-> Result<(), Box<dyn Error>> {
    let statements = many_small_inserts();
    assert_workload_is_crash_safe(
        &SqlWorkload { statements: &statements, harness: HarnessConfig::DEFAULT },
        DurabilityModel::torn_write_requires_sync(),
    )
}

#[test]
fn page_boundary_inserts_survive_a_crash_at_every_write() -> Result<(), Box<dyn Error>> {
    let statements = page_boundary_inserts();
    assert_workload_is_crash_safe(
        &SqlWorkload { statements: &statements, harness: HarnessConfig::DEFAULT },
        DurabilityModel::write_is_durable(),
    )
}

#[test]
fn page_boundary_inserts_survive_a_crash_at_every_write_with_unsynced_writes_lost()
-> Result<(), Box<dyn Error>> {
    let statements = page_boundary_inserts();
    assert_workload_is_crash_safe(
        &SqlWorkload { statements: &statements, harness: HarnessConfig::DEFAULT },
        DurabilityModel::requires_sync(),
    )
}

#[test]
fn page_boundary_inserts_survive_a_crash_at_every_write_with_torn_writes()
-> Result<(), Box<dyn Error>> {
    let statements = page_boundary_inserts();
    assert_workload_is_crash_safe(
        &SqlWorkload { statements: &statements, harness: HarnessConfig::DEFAULT },
        DurabilityModel::torn_write(),
    )
}

#[test]
fn page_boundary_inserts_survive_a_crash_at_every_write_with_torn_and_unsynced_writes()
-> Result<(), Box<dyn Error>> {
    let statements = page_boundary_inserts();
    assert_workload_is_crash_safe(
        &SqlWorkload { statements: &statements, harness: HarnessConfig::DEFAULT },
        DurabilityModel::torn_write_requires_sync(),
    )
}

#[test]
fn create_table_then_inserts_survive_a_crash_at_every_write() -> Result<(), Box<dyn Error>> {
    let statements = create_table_then_inserts();
    assert_workload_is_crash_safe(
        &SqlWorkload { statements: &statements, harness: HarnessConfig::DEFAULT },
        DurabilityModel::write_is_durable(),
    )
}

#[test]
fn create_table_then_inserts_survive_a_crash_at_every_write_with_unsynced_writes_lost()
-> Result<(), Box<dyn Error>> {
    let statements = create_table_then_inserts();
    assert_workload_is_crash_safe(
        &SqlWorkload { statements: &statements, harness: HarnessConfig::DEFAULT },
        DurabilityModel::requires_sync(),
    )
}

#[test]
fn create_table_then_inserts_survive_a_crash_at_every_write_with_torn_writes()
-> Result<(), Box<dyn Error>> {
    let statements = create_table_then_inserts();
    assert_workload_is_crash_safe(
        &SqlWorkload { statements: &statements, harness: HarnessConfig::DEFAULT },
        DurabilityModel::torn_write(),
    )
}

#[test]
fn create_table_then_inserts_survive_a_crash_at_every_write_with_torn_and_unsynced_writes()
-> Result<(), Box<dyn Error>> {
    let statements = create_table_then_inserts();
    assert_workload_is_crash_safe(
        &SqlWorkload { statements: &statements, harness: HarnessConfig::DEFAULT },
        DurabilityModel::torn_write_requires_sync(),
    )
}

#[test]
fn interleaved_allocation_and_catalog_writes_survive_a_crash_at_every_write()
-> Result<(), Box<dyn Error>> {
    let statements = interleaved_allocation_and_catalog_writes();
    assert_workload_is_crash_safe(
        &SqlWorkload { statements: &statements, harness: HarnessConfig::DEFAULT },
        DurabilityModel::write_is_durable(),
    )
}

#[test]
fn interleaved_allocation_and_catalog_writes_survive_a_crash_at_every_write_with_unsynced_writes_lost()
-> Result<(), Box<dyn Error>> {
    let statements = interleaved_allocation_and_catalog_writes();
    assert_workload_is_crash_safe(
        &SqlWorkload { statements: &statements, harness: HarnessConfig::DEFAULT },
        DurabilityModel::requires_sync(),
    )
}

#[test]
fn interleaved_allocation_and_catalog_writes_survive_a_crash_at_every_write_with_torn_writes()
-> Result<(), Box<dyn Error>> {
    let statements = interleaved_allocation_and_catalog_writes();
    assert_workload_is_crash_safe(
        &SqlWorkload { statements: &statements, harness: HarnessConfig::DEFAULT },
        DurabilityModel::torn_write(),
    )
}

#[test]
fn interleaved_allocation_and_catalog_writes_survive_a_crash_at_every_write_with_torn_and_unsynced_writes()
-> Result<(), Box<dyn Error>> {
    let statements = interleaved_allocation_and_catalog_writes();
    assert_workload_is_crash_safe(
        &SqlWorkload { statements: &statements, harness: HarnessConfig::DEFAULT },
        DurabilityModel::torn_write_requires_sync(),
    )
}

#[test]
fn a_kill_mid_transaction_before_commit_leaves_no_trace_at_every_write()
-> Result<(), Box<dyn Error>> {
    let statements = mid_transaction_kill();
    assert_workload_is_crash_safe(
        &SqlWorkload { statements: &statements, harness: HarnessConfig::DEFAULT },
        DurabilityModel::write_is_durable(),
    )
}

#[test]
fn a_kill_mid_transaction_before_commit_leaves_no_trace_at_every_write_with_unsynced_writes_lost()
-> Result<(), Box<dyn Error>> {
    let statements = mid_transaction_kill();
    assert_workload_is_crash_safe(
        &SqlWorkload { statements: &statements, harness: HarnessConfig::DEFAULT },
        DurabilityModel::requires_sync(),
    )
}

#[test]
fn a_kill_mid_transaction_before_commit_leaves_no_trace_at_every_write_with_torn_writes()
-> Result<(), Box<dyn Error>> {
    let statements = mid_transaction_kill();
    assert_workload_is_crash_safe(
        &SqlWorkload { statements: &statements, harness: HarnessConfig::DEFAULT },
        DurabilityModel::torn_write(),
    )
}

#[test]
fn a_kill_mid_transaction_before_commit_leaves_no_trace_at_every_write_with_torn_and_unsynced_writes()
-> Result<(), Box<dyn Error>> {
    let statements = mid_transaction_kill();
    assert_workload_is_crash_safe(
        &SqlWorkload { statements: &statements, harness: HarnessConfig::DEFAULT },
        DurabilityModel::torn_write_requires_sync(),
    )
}

#[test]
fn segment_rolling_inserts_actually_roll_and_truncate() -> Result<(), Box<dyn Error>> {
    assert_segment_rolling_and_truncation_occurred(
        &segment_rolling_inserts(),
        HarnessConfig::SEGMENT_ROLLING,
    )
}

#[test]
fn segment_rolling_inserts_survive_a_crash_at_every_write() -> Result<(), Box<dyn Error>> {
    let statements = segment_rolling_inserts();
    assert_workload_is_crash_safe(
        &SqlWorkload { statements: &statements, harness: HarnessConfig::SEGMENT_ROLLING },
        DurabilityModel::write_is_durable(),
    )
}

#[test]
fn segment_rolling_inserts_survive_a_crash_at_every_write_with_unsynced_writes_lost()
-> Result<(), Box<dyn Error>> {
    let statements = segment_rolling_inserts();
    assert_workload_is_crash_safe(
        &SqlWorkload { statements: &statements, harness: HarnessConfig::SEGMENT_ROLLING },
        DurabilityModel::requires_sync(),
    )
}

#[test]
fn segment_rolling_inserts_survive_a_crash_at_every_write_with_torn_writes()
-> Result<(), Box<dyn Error>> {
    let statements = segment_rolling_inserts();
    assert_workload_is_crash_safe(
        &SqlWorkload { statements: &statements, harness: HarnessConfig::SEGMENT_ROLLING },
        DurabilityModel::torn_write(),
    )
}

#[test]
fn segment_rolling_inserts_survive_a_crash_at_every_write_with_torn_and_unsynced_writes()
-> Result<(), Box<dyn Error>> {
    let statements = segment_rolling_inserts();
    assert_workload_is_crash_safe(
        &SqlWorkload { statements: &statements, harness: HarnessConfig::SEGMENT_ROLLING },
        DurabilityModel::torn_write_requires_sync(),
    )
}
