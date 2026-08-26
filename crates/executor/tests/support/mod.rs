use catalog::Catalog;
use executor::{Executor, ExecutorContext};
use planner::BoundExpr;
use storage::buffer::BufferPool;
use storage::disk::DiskManager;
use storage::dwb::DoubleWriteBuffer;
use storage::page::PAGE_SIZE;
use storage::replacer::LruKReplacer;
use storage::wal::LogManager;
use txn::Transaction;
use types::{MemcomparableEncode, Tuple, Value};

#[cfg(test)]
pub fn open_pool(pool_size: usize) -> (BufferPool, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("create temp dir");
    let disk = DiskManager::open(dir.path().join("test.db"), PAGE_SIZE).expect("open disk");
    let dwb = DoubleWriteBuffer::open(
        dir.path().join("test.db.dwb"),
        DoubleWriteBuffer::DEFAULT_CAPACITY,
    )
    .expect("open double-write buffer");
    let log = LogManager::open(dir.path().join("test.db.wal")).expect("open log");
    let replacer = Box::new(LruKReplacer::new(pool_size, 2));
    (BufferPool::new(disk, dwb, log, pool_size, replacer), dir)
}

#[cfg(test)]
pub fn run_to_completion(
    pool: &BufferPool,
    catalog: &Catalog,
    txn: &Transaction,
    executor: &mut dyn Executor,
) -> Vec<Tuple> {
    let mut ctx = ExecutorContext::new(catalog, pool, txn);
    executor.init(&mut ctx).expect("init");
    let mut rows = Vec::new();
    while let Some(tuple) = executor.next(&mut ctx).expect("next") {
        rows.push(tuple);
    }
    rows
}

#[cfg(test)]
pub fn row(values: Vec<Value>) -> Vec<BoundExpr> {
    values.into_iter().map(BoundExpr::Literal).collect()
}

#[cfg(test)]
pub fn encoded(value: &Value) -> Vec<u8> {
    let mut buf = Vec::new();
    value.encode_memcomparable(&mut buf).expect("encode");
    buf
}
