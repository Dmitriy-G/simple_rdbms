use catalog::{Catalog, Column, Schema};
use common::{DbConfig, Error, Result, TxnId};
use executor::ExecutorContext;
use planner::{Binder, BoundStatement, PhysicalPlan, to_physical};
use sql::{Lexer, Parser};
use storage::buffer::BufferPool;
use storage::disk::DiskManager;
use storage::replacer::LruKReplacer;
use txn::{IsolationLevel, Transaction, TransactionManager};
use types::{Tuple, Value};

use crate::executor_factory::build_executor;
use crate::result_set::ResultSet;

/// The number of recent accesses the buffer pool's `LruKReplacer` tracks per
/// frame before falling back to plain LRU. Not yet exposed as a config knob;
/// `2` is the classic LRU-2 choice.
const REPLACER_K: usize = 2;

/// A single open database. Owns the catalog, buffer pool, and transaction
/// manager, and is the object every SQL statement is executed against.
pub struct Database {
    catalog: Catalog,
    buffer_pool: BufferPool,
    #[allow(dead_code)]
    txn_manager: TransactionManager,
}

impl Database {
    /// Opens (creating if necessary) the database described by `config`,
    /// standing up its disk manager, buffer pool, and catalog.
    pub fn open(config: DbConfig) -> Result<Self> {
        let disk_manager = DiskManager::open(config.db_path.clone(), config.page_size)?;
        let replacer = Box::new(LruKReplacer::new(config.buffer_pool_size, REPLACER_K));
        let buffer_pool = BufferPool::new(disk_manager, config.buffer_pool_size, replacer);
        let catalog = Catalog::open(&buffer_pool)?;
        Ok(Self { catalog, buffer_pool, txn_manager: TransactionManager::new() })
    }

    /// Closes the database: flushes every dirty page and syncs the file to
    /// durable storage. Errors here are real failures worth reporting, which
    /// is why this is a separate, explicit call rather than left entirely to
    /// `Drop` (which cannot report them).
    pub fn close(self) -> Result<()> {
        self.buffer_pool.flush_all()?;
        self.buffer_pool.sync()?;
        Ok(())
    }

    /// Parses, binds, plans, and executes one SQL statement, returning its
    /// result set.
    pub fn execute(&mut self, sql: &str) -> Result<ResultSet> {
        let tokens = Lexer::new(sql).tokenize().map_err(|err| Error::Parse(err.render(sql)))?;
        let statement = Parser::new(tokens).parse().map_err(|err| Error::Parse(err.render(sql)))?;
        let bound = Binder::new(&self.catalog).bind(statement)?;

        match bound {
            BoundStatement::CreateTable(create) => {
                let schema = Schema::new(
                    create
                        .columns
                        .into_iter()
                        .map(|column| Column::new(column.name, column.data_type, column.nullable))
                        .collect(),
                );
                self.catalog.create_table(&self.buffer_pool, &create.table_name, schema)?;
                Ok(ResultSet::rows_affected(0))
            }
            BoundStatement::Insert(insert) => {
                let physical = to_physical(planner::plan(BoundStatement::Insert(insert))?);
                let rows = self.run(physical)?;
                let inserted = rows
                    .first()
                    .and_then(|tuple| tuple.values().first())
                    .and_then(|value| match value {
                        Value::BigInt(count) => Some(*count as usize),
                        _ => None,
                    })
                    .unwrap_or(0);
                Ok(ResultSet::rows_affected(inserted))
            }
            BoundStatement::Select(select) => {
                let column_names = select.column_names.clone();
                let physical = to_physical(planner::plan(BoundStatement::Select(select))?);
                let rows = self.run(physical)?;
                Ok(ResultSet::rows(column_names, rows))
            }
        }
    }

    /// Every registered table's name, sorted for a deterministic order.
    pub fn table_names(&self) -> Vec<String> {
        self.catalog.table_names().into_iter().map(String::from).collect()
    }

    /// The column schema of the table named `name`.
    pub fn table_schema(&self, name: &str) -> Result<Schema> {
        Ok(self.catalog.get_table(name)?.schema.clone())
    }

    /// Builds and drains an executor tree for `physical`, collecting every
    /// tuple it produces. There is no persistent transaction context wired
    /// up yet (that arrives with M7), so each call runs under a throwaway,
    /// never-committed `Transaction` purely to satisfy `ExecutorContext`'s
    /// shape.
    fn run(&mut self, physical: PhysicalPlan) -> Result<Vec<Tuple>> {
        let txn = Transaction::new(TxnId(0), IsolationLevel::ReadCommitted);
        let mut executor = build_executor(physical);
        let mut ctx = ExecutorContext::new(&self.catalog, &mut self.buffer_pool, &txn);
        executor.init(&mut ctx)?;

        let mut rows = Vec::new();
        while let Some(tuple) = executor.next(&mut ctx)? {
            rows.push(tuple);
        }
        Ok(rows)
    }
}

impl Drop for Database {
    /// Best-effort flush on drop, for callers that don't call `close`
    /// explicitly. Errors are swallowed here (there is nowhere to report
    /// them from `Drop`); call `close` to observe them.
    fn drop(&mut self) {
        let _ = self.buffer_pool.flush_all();
    }
}
