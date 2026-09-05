use catalog::{Catalog, Column, Schema};
use common::TxnId;
use executor::{Executor, ExecutorContext, IndexScanExecutor, InsertExecutor, SeqScanExecutor};
use planner::BoundExpr;
use storage::buffer::BufferPool;
use storage::heap::TableHeap;
use test_support::PoolOptions;
use txn::{IsolationLevel, TransactionManager};
use types::{DataType, Encode, Tuple, Value};

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
    let mut catalog = Catalog::new();
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
    let mut catalog = Catalog::new();
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
    let mut catalog = Catalog::new();
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
    let mut catalog = Catalog::new();
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
