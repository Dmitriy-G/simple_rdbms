mod support;

use catalog::{Catalog, Column, Schema};
use common::TxnId;
use executor::{Executor, ExecutorContext, InsertExecutor};
use storage::btree::BTreeIndex;
use storage::heap::TableHeap;
use support::{encoded, row};
use txn::{IsolationLevel, Transaction};
use types::{DataType, Value};

const TXN: TxnId = TxnId(0);

#[test]
fn insert_maintains_a_single_index() {
    let (pool, _dir) = support::open_pool(16);
    let mut catalog = Catalog::new();

    let schema = Schema::new(vec![Column::new("id", DataType::Integer, true)]);
    let table_id = catalog.create_table(&pool, TXN, "t", schema).expect("create table").table_id;
    let index_id =
        catalog.create_index(&pool, TXN, "idx_t_id", table_id, 0).expect("create index").index_id;

    let rows = vec![
        row(vec![Value::Integer(1)]),
        row(vec![Value::Integer(2)]),
        row(vec![Value::Integer(3)]),
    ];
    let mut insert = InsertExecutor::new(table_id, rows);
    let txn = Transaction::new(TXN, IsolationLevel::ReadCommitted);
    support::run_to_completion(&pool, &catalog, &txn, &mut insert);

    let root_page_id = catalog.index_root_page(&pool, index_id).expect("root page");
    let index = BTreeIndex::open(&pool, root_page_id);
    for v in [1, 2, 3] {
        let key = encoded(&Value::Integer(v));
        let rids = index.get(&key).expect("get");
        assert_eq!(rids.len(), 1, "value {v} should be indexed exactly once");
    }
}

#[test]
fn insert_maintains_every_index_on_a_multi_indexed_table() {
    let (pool, _dir) = support::open_pool(16);
    let mut catalog = Catalog::new();

    let schema = Schema::new(vec![
        Column::new("a", DataType::Integer, true),
        Column::new("b", DataType::Integer, true),
    ]);
    let table_id = catalog.create_table(&pool, TXN, "t", schema).expect("create table").table_id;
    let idx_a =
        catalog.create_index(&pool, TXN, "idx_t_a", table_id, 0).expect("create index a").index_id;
    let idx_b =
        catalog.create_index(&pool, TXN, "idx_t_b", table_id, 1).expect("create index b").index_id;

    let rows = vec![row(vec![Value::Integer(10), Value::Integer(100)])];
    let mut insert = InsertExecutor::new(table_id, rows);
    let txn = Transaction::new(TXN, IsolationLevel::ReadCommitted);
    support::run_to_completion(&pool, &catalog, &txn, &mut insert);

    let root_a = catalog.index_root_page(&pool, idx_a).expect("root a");
    let root_b = catalog.index_root_page(&pool, idx_b).expect("root b");
    assert_eq!(
        BTreeIndex::open(&pool, root_a).get(&encoded(&Value::Integer(10))).unwrap().len(),
        1
    );
    assert_eq!(
        BTreeIndex::open(&pool, root_b).get(&encoded(&Value::Integer(100))).unwrap().len(),
        1
    );
}

#[test]
fn a_failed_index_insert_returns_an_error_and_leaves_that_row_out_of_the_index() {
    let (pool, _dir) = support::open_pool(16);
    let mut catalog = Catalog::new();

    let schema = Schema::new(vec![Column::new("val", DataType::Double, true)]);
    let table = catalog.create_table(&pool, TXN, "t", schema).expect("create table");
    let table_id = table.table_id;
    let first_page_id = table.first_page_id;
    let index_id =
        catalog.create_index(&pool, TXN, "idx_t_val", table_id, 0).expect("create index").index_id;

    let rows = vec![row(vec![Value::Double(1.0)]), row(vec![Value::Double(f64::NAN)])];
    let mut insert = InsertExecutor::new(table_id, rows);
    let txn = Transaction::new(TXN, IsolationLevel::ReadCommitted);
    let mut ctx = ExecutorContext::new(&catalog, &pool, &txn);
    insert.init(&mut ctx).expect("init");
    let result = insert.next(&mut ctx);
    assert!(result.is_err(), "an unorderable indexed value must fail the statement");

    let heap_row_count = TableHeap::open(&pool, first_page_id).iter().count();
    assert_eq!(
        heap_row_count, 2,
        "both heap rows are written before the failing index insert is attempted; a higher \
         layer (engine) is responsible for rolling this back via WAL undo"
    );

    let root_page_id = catalog.index_root_page(&pool, index_id).expect("root page");
    let index = BTreeIndex::open(&pool, root_page_id);
    assert_eq!(index.get(&encoded(&Value::Double(1.0))).unwrap().len(), 1);
}
