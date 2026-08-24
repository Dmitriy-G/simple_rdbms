use catalog::Catalog;
use storage::buffer::BufferPool;
use txn::Transaction;

pub struct ExecutorContext<'a> {
    pub catalog: &'a Catalog,
    pub buffer_pool: &'a BufferPool,
    pub txn: &'a Transaction,
}

impl<'a> ExecutorContext<'a> {
    pub fn new(catalog: &'a Catalog, buffer_pool: &'a BufferPool, txn: &'a Transaction) -> Self {
        Self { catalog, buffer_pool, txn }
    }
}
