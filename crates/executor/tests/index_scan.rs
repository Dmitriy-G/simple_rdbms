mod support;

use catalog::{Catalog, Column, Schema};
use common::{IndexId, TableId, TxnId};
use executor::{IndexScanExecutor, InsertExecutor};
use storage::buffer::BufferPool;
use support::{encoded, row};
use txn::{IsolationLevel, Transaction};
use types::{DataType, Value};

const TXN: TxnId = TxnId(0);

#[cfg(test)]
fn seed(pool: &BufferPool, count: i32) -> (Catalog, TableId, IndexId) {
    let mut catalog = Catalog::new();
    let schema = Schema::new(vec![Column::new("id", DataType::Integer, true)]);
    let table_id = catalog.create_table(pool, TXN, "t", schema).expect("create table").table_id;
    let index_id =
        catalog.create_index(pool, TXN, "idx_t_id", table_id, 0).expect("create index").index_id;

    let rows = (0..count).map(|i| row(vec![Value::Integer(i)])).collect();
    let mut insert = InsertExecutor::new(table_id, rows);
    let txn = Transaction::new(TXN, IsolationLevel::ReadCommitted);
    support::run_to_completion(pool, &catalog, &txn, &mut insert);
    (catalog, table_id, index_id)
}

#[cfg(test)]
fn scan_values(
    pool: &BufferPool,
    catalog: &Catalog,
    index_id: IndexId,
    table_id: TableId,
    start: Option<Vec<u8>>,
    end: Option<Vec<u8>>,
) -> Vec<i32> {
    let txn = Transaction::new(TXN, IsolationLevel::ReadCommitted);
    let mut scan = IndexScanExecutor::new(index_id, table_id, start, end);
    let tuples = support::run_to_completion(pool, catalog, &txn, &mut scan);
    tuples
        .iter()
        .map(|tuple| match tuple.values() {
            [Value::Integer(v)] => *v,
            other => panic!("expected a single Integer column, got {other:?}"),
        })
        .collect()
}

#[test]
fn index_scan_finds_every_row_in_ascending_key_order() {
    let (pool, _dir) = support::open_pool(16);
    let (catalog, table_id, index_id) = seed(&pool, 50);

    let values = scan_values(&pool, &catalog, index_id, table_id, None, None);
    assert_eq!(values, (0..50).collect::<Vec<_>>());
}

#[test]
fn index_scan_respects_start_and_end_bounds() {
    let (pool, _dir) = support::open_pool(16);
    let (catalog, table_id, index_id) = seed(&pool, 20);

    let start = Some(encoded(&Value::Integer(5)));
    let end = Some(encoded(&Value::Integer(10)));
    let values = scan_values(&pool, &catalog, index_id, table_id, start, end);
    assert_eq!(values, vec![5, 6, 7, 8, 9], "range [5, 10) should yield exactly those keys");
}

#[test]
fn index_scan_returns_every_duplicate_key() {
    let (pool, _dir) = support::open_pool(16);
    let mut catalog = Catalog::new();
    let schema = Schema::new(vec![Column::new("id", DataType::Integer, true)]);
    let table_id = catalog.create_table(&pool, TXN, "t", schema).expect("create table").table_id;
    let index_id =
        catalog.create_index(&pool, TXN, "idx_t_id", table_id, 0).expect("create index").index_id;

    let rows = (0..5).map(|_| row(vec![Value::Integer(7)])).collect();
    let mut insert = InsertExecutor::new(table_id, rows);
    let txn = Transaction::new(TXN, IsolationLevel::ReadCommitted);
    support::run_to_completion(&pool, &catalog, &txn, &mut insert);

    let values = scan_values(&pool, &catalog, index_id, table_id, None, None);
    assert_eq!(values, vec![7, 7, 7, 7, 7]);
}

#[test]
fn index_scan_after_a_root_split_still_finds_every_row() {
    let (pool, _dir) = support::open_pool(64);
    let (catalog, table_id, index_id) = seed(&pool, 3_000);

    let values = scan_values(&pool, &catalog, index_id, table_id, None, None);
    assert_eq!(
        values,
        (0..3_000).collect::<Vec<_>>(),
        "every row must still be reachable through the index after enough inserts to force a \
         root split, proving InsertExecutor persisted the new root page via \
         Catalog::update_index_root_page rather than leaving the catalog pointing at the old one"
    );
}
