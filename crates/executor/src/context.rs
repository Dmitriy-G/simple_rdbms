use catalog::Catalog;
use storage::buffer::BufferPool;
use txn::Transaction;

/// The shared state every operator in a plan tree needs while executing:
/// the catalog for schema lookups, the buffer pool for reading and writing
/// pages, and the transaction on whose behalf the plan is running (for
/// visibility checks and lock acquisition). Passed down through `next`
/// calls rather than owned by individual operators, so the same context
/// serves the whole tree.
pub struct ExecutorContext<'a> {
    /// The catalog, for resolving table/column metadata during execution.
    pub catalog: &'a Catalog,
    /// The buffer pool backing every page read or write an operator makes.
    pub buffer_pool: &'a mut BufferPool,
    /// The transaction this execution is running under.
    pub txn: &'a Transaction,
}

impl<'a> ExecutorContext<'a> {
    /// Builds an executor context from its parts.
    pub fn new(
        catalog: &'a Catalog,
        buffer_pool: &'a mut BufferPool,
        txn: &'a Transaction,
    ) -> Self {
        Self { catalog, buffer_pool, txn }
    }
}
