use catalog::Catalog;
use storage::buffer::BufferPool;
use txn::{LockManager, Transaction};

pub struct ExecutorContext<'a> {
    pub catalog: &'a Catalog,
    pub buffer_pool: &'a BufferPool,
    pub txn: &'a Transaction,
    pub lock_manager: &'a LockManager,
}

impl<'a> ExecutorContext<'a> {
    pub fn new(
        catalog: &'a Catalog,
        buffer_pool: &'a BufferPool,
        txn: &'a Transaction,
        lock_manager: &'a LockManager,
    ) -> Self {
        Self { catalog, buffer_pool, txn, lock_manager }
    }
}
