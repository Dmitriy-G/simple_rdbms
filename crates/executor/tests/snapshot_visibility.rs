use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{RecvTimeoutError, channel};
use std::thread;
use std::time::Duration;

use catalog::{Catalog, Column, Schema};
use common::TxnId;
use executor::{Executor, ExecutorContext, IndexScanExecutor, InsertExecutor, SeqScanExecutor};
use planner::BoundExpr;
use storage::buffer::BufferPool;
use storage::heap::TableHeap;
use test_support::PoolOptions;
use txn::{IsolationLevel, TransactionManager};
use types::{DataType, Encode, Tuple, Value};

const RACE_INSERT_COUNT: i32 = 300;
const RACE_TEST_TIMEOUT: Duration = Duration::from_secs(30);

#[cfg(test)]
fn open_pool(pool_size: usize) -> (BufferPool, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("create temp dir");
    let pool = test_support::open_pool(dir.path(), PoolOptions::new(pool_size)).expect("open pool");
    (pool, dir)
}

#[cfg(test)]
fn row(value: i32) -> Vec<BoundExpr> {
    vec![BoundExpr::Literal(Value::Integer(value))]
}

#[cfg(test)]
fn as_integer(tuple: &Tuple) -> i32 {
    match tuple.values() {
        [Value::Integer(v)] => *v,
        other => panic!("expected a single Integer column, got {other:?}"),
    }
}

#[cfg(test)]
fn insert_row(
    txn_manager: &mut TransactionManager,
    pool: &storage::buffer::BufferPool,
    catalog: &Catalog,
    table_id: common::TableId,
    value: i32,
) -> TxnId {
    let writer = txn_manager.begin(pool, IsolationLevel::ReadCommitted).expect("begin writer");
    let writer_txn = txn_manager.get(writer).expect("writer txn").clone();
    let lock_manager = txn_manager.lock_manager().clone();
    let version_store = txn_manager.version_store().clone();
    let mut insert = InsertExecutor::new(table_id, vec![row(value)]);
    let mut ctx = ExecutorContext::new(catalog, pool, &writer_txn, &lock_manager, &version_store);
    insert.init(&mut ctx).expect("init insert");
    insert.next(&mut ctx).expect("insert row");
    txn_manager.commit(writer, pool).expect("commit writer");
    writer
}

#[test]
fn seq_scan_under_snapshot_isolation_hides_uncommitted_rows_but_shows_untracked_ones() {
    let (pool, _dir) = open_pool(16);
    let catalog = Catalog::new();
    let mut txn_manager = TransactionManager::new(None);

    let ddl_txn = txn_manager.begin(&pool, IsolationLevel::ReadCommitted).expect("begin ddl");
    let schema = Schema::new(vec![Column::new("n", DataType::Integer, true)]);
    let table = catalog.create_table(&pool, ddl_txn, "t", schema).expect("create table");
    let table_id = table.table_id;
    txn_manager.commit(ddl_txn, &pool).expect("commit ddl");

    let mut heap = TableHeap::open(&pool, table.first_page_id);
    let mut bytes = Vec::new();
    Tuple::new(vec![Value::Integer(0)]).encode(&mut bytes);
    heap.insert_tuple(ddl_txn, &bytes).expect("seed a row the version store never heard of");

    let reader = txn_manager.begin(&pool, IsolationLevel::SnapshotIsolation).expect("begin reader");
    let reader_txn = txn_manager.get(reader).expect("reader txn").clone();

    insert_row(&mut txn_manager, &pool, &catalog, table_id, 1);

    let lock_manager = txn_manager.lock_manager().clone();
    let version_store = txn_manager.version_store().clone();
    let mut scan_ctx =
        ExecutorContext::new(&catalog, &pool, &reader_txn, &lock_manager, &version_store);
    let mut scan = SeqScanExecutor::new(table_id);
    scan.init(&mut scan_ctx).expect("init scan");

    let mut seen = Vec::new();
    while let Some(tuple) = scan.next(&mut scan_ctx).expect("next") {
        seen.push(as_integer(&tuple));
    }
    assert_eq!(
        seen,
        vec![0],
        "the reader must see the row the version store never heard of but not the one \
         committed after its snapshot began"
    );
}

#[test]
fn seq_scan_under_snapshot_isolation_sees_its_own_uncommitted_insert() {
    let (pool, _dir) = open_pool(16);
    let catalog = Catalog::new();
    let mut txn_manager = TransactionManager::new(None);

    let ddl_txn = txn_manager.begin(&pool, IsolationLevel::ReadCommitted).expect("begin ddl");
    let schema = Schema::new(vec![Column::new("n", DataType::Integer, true)]);
    let table_id =
        catalog.create_table(&pool, ddl_txn, "t", schema).expect("create table").table_id;
    txn_manager.commit(ddl_txn, &pool).expect("commit ddl");

    let writer = txn_manager.begin(&pool, IsolationLevel::SnapshotIsolation).expect("begin writer");
    let writer_txn = txn_manager.get(writer).expect("writer txn").clone();
    let lock_manager = txn_manager.lock_manager().clone();
    let version_store = txn_manager.version_store().clone();
    let mut insert = InsertExecutor::new(table_id, vec![row(1)]);
    let mut insert_ctx =
        ExecutorContext::new(&catalog, &pool, &writer_txn, &lock_manager, &version_store);
    insert.init(&mut insert_ctx).expect("init insert");
    insert.next(&mut insert_ctx).expect("insert row");

    let mut scan_ctx =
        ExecutorContext::new(&catalog, &pool, &writer_txn, &lock_manager, &version_store);
    let mut scan = SeqScanExecutor::new(table_id);
    scan.init(&mut scan_ctx).expect("init scan");

    let mut seen = Vec::new();
    while let Some(tuple) = scan.next(&mut scan_ctx).expect("next") {
        seen.push(as_integer(&tuple));
    }
    assert_eq!(
        seen,
        vec![1],
        "a transaction must see its own uncommitted insert even though nobody has committed it"
    );
}

#[test]
fn seq_scan_under_read_committed_still_takes_its_locks() {
    let (pool, _dir) = open_pool(16);
    let catalog = Catalog::new();
    let mut txn_manager = TransactionManager::new(None);

    let ddl_txn = txn_manager.begin(&pool, IsolationLevel::ReadCommitted).expect("begin ddl");
    let schema = Schema::new(vec![Column::new("n", DataType::Integer, true)]);
    let table_id =
        catalog.create_table(&pool, ddl_txn, "t", schema).expect("create table").table_id;
    txn_manager.commit(ddl_txn, &pool).expect("commit ddl");

    insert_row(&mut txn_manager, &pool, &catalog, table_id, 7);

    let reader = txn_manager.begin(&pool, IsolationLevel::ReadCommitted).expect("begin reader");
    let reader_txn = txn_manager.get(reader).expect("reader txn").clone();
    let lock_manager = txn_manager.lock_manager().clone();
    let version_store = txn_manager.version_store().clone();
    let mut scan_ctx =
        ExecutorContext::new(&catalog, &pool, &reader_txn, &lock_manager, &version_store);
    let mut scan = SeqScanExecutor::new(table_id);
    scan.init(&mut scan_ctx).expect("init scan");
    while scan.next(&mut scan_ctx).expect("next").is_some() {}

    assert!(
        lock_manager.held_lock_count(reader) > 0,
        "a ReadCommitted scan must still hold locks after reading every row"
    );
}

#[test]
fn index_scan_under_snapshot_isolation_hides_the_same_invisible_row() {
    let (pool, _dir) = open_pool(16);
    let catalog = Catalog::new();
    let mut txn_manager = TransactionManager::new(None);

    let ddl_txn = txn_manager.begin(&pool, IsolationLevel::ReadCommitted).expect("begin ddl");
    let schema = Schema::new(vec![Column::new("id", DataType::Integer, true)]);
    let table_id =
        catalog.create_table(&pool, ddl_txn, "t", schema).expect("create table").table_id;
    let index_id = catalog
        .create_index(&pool, ddl_txn, "idx_t_id", table_id, 0)
        .expect("create index")
        .index_id;
    txn_manager.commit(ddl_txn, &pool).expect("commit ddl");

    insert_row(&mut txn_manager, &pool, &catalog, table_id, 1);

    let reader = txn_manager.begin(&pool, IsolationLevel::SnapshotIsolation).expect("begin reader");
    let reader_txn = txn_manager.get(reader).expect("reader txn").clone();

    insert_row(&mut txn_manager, &pool, &catalog, table_id, 2);

    let lock_manager = txn_manager.lock_manager().clone();
    let version_store = txn_manager.version_store().clone();
    let mut scan_ctx =
        ExecutorContext::new(&catalog, &pool, &reader_txn, &lock_manager, &version_store);
    let mut scan = IndexScanExecutor::new(index_id, table_id, None, None);
    scan.init(&mut scan_ctx).expect("init scan");

    let mut seen = Vec::new();
    while let Some(tuple) = scan.next(&mut scan_ctx).expect("next") {
        seen.push(as_integer(&tuple));
    }
    assert_eq!(
        seen,
        vec![1],
        "the row committed before the reader's snapshot must be visible, the one committed \
         after it must not"
    );
}

#[test]
fn insert_registers_a_version_chain_before_the_rid_is_returned() {
    let (pool, _dir) = open_pool(16);
    let catalog = Catalog::new();
    let mut txn_manager = TransactionManager::new(None);

    let ddl_txn = txn_manager.begin(&pool, IsolationLevel::ReadCommitted).expect("begin ddl");
    let schema = Schema::new(vec![Column::new("n", DataType::Integer, true)]);
    let table = catalog.create_table(&pool, ddl_txn, "t", schema).expect("create table");
    let table_id = table.table_id;
    let first_page_id = table.first_page_id;
    txn_manager.commit(ddl_txn, &pool).expect("commit ddl");

    let observer =
        txn_manager.begin(&pool, IsolationLevel::SnapshotIsolation).expect("begin observer");
    let observer_txn = txn_manager.get(observer).expect("observer txn").clone();

    let writer = txn_manager.begin(&pool, IsolationLevel::ReadCommitted).expect("begin writer");
    let writer_txn = txn_manager.get(writer).expect("writer txn").clone();
    let lock_manager = txn_manager.lock_manager().clone();
    let version_store = txn_manager.version_store().clone();
    let mut insert = InsertExecutor::new(table_id, vec![row(42)]);
    let mut insert_ctx =
        ExecutorContext::new(&catalog, &pool, &writer_txn, &lock_manager, &version_store);
    insert.init(&mut insert_ctx).expect("init insert");
    insert.next(&mut insert_ctx).expect("insert row");

    let heap = TableHeap::open(&pool, first_page_id);
    let (rid, _bytes) = heap
        .iter()
        .next()
        .expect("the row just inserted must already be readable from the heap")
        .expect("read the row just inserted");

    assert!(
        !version_store.is_visible_to(rid, observer_txn.txn_id, observer_txn.read_ts),
        "the inserted row's version chain must already exist and hide it from a snapshot \
         opened before the insert, the instant the insert call returns"
    );
    assert!(
        version_store.is_visible_to(rid, writer_txn.txn_id, writer_txn.read_ts),
        "the inserting transaction must see its own uncommitted insert"
    );
}

#[test]
fn seq_scan_under_snapshot_isolation_never_observes_a_row_before_its_version_chain_exists() {
    let (pool, _dir) = open_pool(16);
    let pool = Arc::new(pool);
    let catalog = Catalog::new();
    let mut txn_manager = TransactionManager::new(None);

    let ddl_txn = txn_manager.begin(&pool, IsolationLevel::ReadCommitted).expect("begin ddl");
    let schema = Schema::new(vec![Column::new("n", DataType::Integer, true)]);
    let table_id =
        catalog.create_table(&pool, ddl_txn, "t", schema).expect("create table").table_id;
    txn_manager.commit(ddl_txn, &pool).expect("commit ddl");
    let catalog = Arc::new(catalog);

    let writer = txn_manager.begin(&pool, IsolationLevel::ReadCommitted).expect("begin writer");
    let writer_txn = Arc::new(txn_manager.get(writer).expect("writer txn").clone());
    let reader = txn_manager.begin(&pool, IsolationLevel::SnapshotIsolation).expect("begin reader");
    let reader_txn = Arc::new(txn_manager.get(reader).expect("reader txn").clone());

    let lock_manager = txn_manager.lock_manager().clone();
    let version_store = txn_manager.version_store().clone();

    let done = Arc::new(AtomicBool::new(false));
    let (violation_tx, violation_rx) = channel::<i32>();
    let (finished_tx, finished_rx) = channel::<()>();

    {
        let pool = Arc::clone(&pool);
        let catalog = Arc::clone(&catalog);
        let writer_txn = Arc::clone(&writer_txn);
        let lock_manager = lock_manager.clone();
        let version_store = version_store.clone();
        let done = Arc::clone(&done);
        thread::spawn(move || {
            let mut ctx =
                ExecutorContext::new(&catalog, &pool, &writer_txn, &lock_manager, &version_store);
            for value in 0..RACE_INSERT_COUNT {
                let mut insert = InsertExecutor::new(table_id, vec![row(value)]);
                insert.init(&mut ctx).expect("init insert");
                insert.next(&mut ctx).expect("insert row");
            }
            done.store(true, Ordering::Release);
        });
    }

    {
        let pool = Arc::clone(&pool);
        let catalog = Arc::clone(&catalog);
        let reader_txn = Arc::clone(&reader_txn);
        let lock_manager = lock_manager.clone();
        let version_store = version_store.clone();
        let done = Arc::clone(&done);
        thread::spawn(move || {
            while !done.load(Ordering::Acquire) {
                let mut scan_ctx = ExecutorContext::new(
                    &catalog,
                    &pool,
                    &reader_txn,
                    &lock_manager,
                    &version_store,
                );
                let mut scan = SeqScanExecutor::new(table_id);
                scan.init(&mut scan_ctx).expect("init scan");
                while let Some(tuple) = scan.next(&mut scan_ctx).expect("next") {
                    let _ = violation_tx.send(as_integer(&tuple));
                }
            }
            let _ = finished_tx.send(());
        });
    }

    match finished_rx.recv_timeout(RACE_TEST_TIMEOUT) {
        Ok(()) => {}
        Err(RecvTimeoutError::Timeout) => {
            panic!("reader thread did not finish within {RACE_TEST_TIMEOUT:?}")
        }
        Err(RecvTimeoutError::Disconnected) => panic!("reader thread panicked before finishing"),
    }

    let violations: Vec<i32> = violation_rx.try_iter().collect();
    assert!(
        violations.is_empty(),
        "a snapshot-isolation scan observed an uncommitted row inserted by another transaction \
         still open: {violations:?}"
    );
}
