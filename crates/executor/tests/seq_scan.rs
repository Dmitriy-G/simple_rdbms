//! `SeqScanExecutor`'s lazy, page-at-a-time cursor: tuples come back in
//! storage order without materializing the whole table, laziness is
//! observable (pulling one tuple never fetches pages beyond the first),
//! and two scans interleaved over the same table each still yield the
//! full, correct result — exercising the buffer pool's sole-owner pin
//! protocol alongside the cursor.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use catalog::{Catalog, Column, Schema};
use common::{TableId, TxnId};
use executor::{Executor, ExecutorContext, SeqScanExecutor};
use storage::buffer::BufferPool;
use storage::disk::DiskManager;
use storage::heap::TableHeap;
use storage::page::PAGE_SIZE;
use storage::replacer::LruKReplacer;
use txn::{IsolationLevel, Transaction};
use types::{DataType, Encode, Tuple, Value};

fn open_pool(pool_size: usize) -> (BufferPool, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("create temp dir");
    let disk = DiskManager::open(dir.path().join("test.db"), PAGE_SIZE).expect("open disk");
    let replacer = Box::new(LruKReplacer::new(pool_size, 2));
    (BufferPool::new(disk, pool_size, replacer), dir)
}

/// Creates a single-column (`n BIGINT`) table and inserts `n` in `0..count`,
/// enough rows to span several pages against a small pool. Returns the
/// table's id and the values expected back, in scan order.
fn seed_table(pool: &BufferPool, catalog: &mut Catalog, count: i64) -> (TableId, Vec<i64>) {
    let schema = Schema::new(vec![Column::new("n", DataType::BigInt, false)]);
    let info = catalog.create_table(pool, "t", schema).expect("create table");
    let table_id = info.table_id;
    let mut heap = TableHeap::open(pool, info.first_page_id);

    let mut expected = Vec::with_capacity(count as usize);
    for n in 0..count {
        let mut bytes = Vec::new();
        Tuple::new(vec![Value::BigInt(n)]).encode(&mut bytes);
        heap.insert_tuple(&bytes).expect("insert tuple");
        expected.push(n);
    }
    (table_id, expected)
}

fn as_bigint(tuple: &Tuple) -> i64 {
    match tuple.values() {
        [Value::BigInt(n)] => *n,
        other => panic!("expected a single BigInt column, got {other:?}"),
    }
}

#[test]
fn scan_spanning_pages_preserves_insertion_order() {
    let (pool, _dir) = open_pool(2);
    let mut catalog = Catalog::new();
    let (table_id, expected) = seed_table(&pool, &mut catalog, 800);

    let txn = Transaction::new(TxnId(0), IsolationLevel::ReadCommitted);
    let mut ctx = ExecutorContext::new(&catalog, &pool, &txn);
    let mut scan = SeqScanExecutor::new(table_id);
    scan.init(&mut ctx).expect("init");

    let mut rows = Vec::new();
    while let Some(tuple) = scan.next(&mut ctx).expect("next") {
        rows.push(as_bigint(&tuple));
    }
    assert_eq!(rows, expected);
}

#[test]
fn pulling_one_tuple_never_fetches_pages_beyond_the_first() {
    let (pool, _dir) = open_pool(4);
    let mut catalog = Catalog::new();
    // Comfortably more than fit on one page, so a full scan would touch
    // several pages if it weren't lazy.
    let (table_id, _expected) = seed_table(&pool, &mut catalog, 800);

    let txn = Transaction::new(TxnId(0), IsolationLevel::ReadCommitted);
    let mut ctx = ExecutorContext::new(&catalog, &pool, &txn);
    let mut scan = SeqScanExecutor::new(table_id);
    scan.init(&mut ctx).expect("init");

    pool.reset_fetch_count();
    let first = scan.next(&mut ctx).expect("next");
    assert!(first.is_some(), "expected at least one row");
    assert_eq!(
        pool.fetch_count(),
        1,
        "pulling a single tuple should fetch exactly the page it lives on"
    );
}

#[test]
fn interleaved_scans_over_the_same_table_each_yield_the_full_result() {
    let (pool, _dir) = open_pool(2);
    let mut catalog = Catalog::new();
    let (table_id, expected) = seed_table(&pool, &mut catalog, 800);

    let txn = Transaction::new(TxnId(0), IsolationLevel::ReadCommitted);
    let mut ctx = ExecutorContext::new(&catalog, &pool, &txn);

    let mut scan_a = SeqScanExecutor::new(table_id);
    let mut scan_b = SeqScanExecutor::new(table_id);
    scan_a.init(&mut ctx).expect("init a");
    scan_b.init(&mut ctx).expect("init b");

    let mut rows_a = Vec::new();
    let mut rows_b = Vec::new();
    loop {
        let next_a = scan_a.next(&mut ctx).expect("next a");
        let next_b = scan_b.next(&mut ctx).expect("next b");
        let (done_a, done_b) = (next_a.is_none(), next_b.is_none());
        if let Some(tuple) = next_a {
            rows_a.push(as_bigint(&tuple));
        }
        if let Some(tuple) = next_b {
            rows_b.push(as_bigint(&tuple));
        }
        if done_a && done_b {
            break;
        }
    }

    assert_eq!(rows_a, expected);
    assert_eq!(rows_b, expected);
}
