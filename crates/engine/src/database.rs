use common::{Error, Result};

use catalog::Catalog;
use common::DbConfig;
use txn::TransactionManager;

use crate::result_set::ResultSet;

/// A single open database. Owns the catalog and transaction manager, and is
/// the object every SQL statement is executed against.
///
/// `open`/`close` are real (if minimal) plumbing rather than stubs: they
/// stand up the in-memory catalog and transaction manager so the facade
/// exists to call into. The `storage::DiskManager`/`BufferPool` wiring they
/// will eventually drive is itself still stubbed (see `storage::disk` and
/// `storage::buffer`), so `open` does not yet touch the database file on
/// disk. `execute` reports that honestly: every statement returns
/// `Error::NotSupported` until the SQL/planner/executor pipeline is wired
/// up in later milestones (see `docs/ROADMAP.md`), rather than silently
/// pretending to succeed.
pub struct Database {
    #[allow(dead_code)]
    config: DbConfig,
    #[allow(dead_code)]
    catalog: Catalog,
    #[allow(dead_code)]
    txn_manager: TransactionManager,
}

impl Database {
    /// Opens (or creates) the database described by `config`.
    pub fn open(config: DbConfig) -> Result<Self> {
        Ok(Self { config, catalog: Catalog::new(), txn_manager: TransactionManager::new() })
    }

    /// Closes the database, flushing any pending state.
    pub fn close(self) -> Result<()> {
        Ok(())
    }

    /// Parses, binds, plans, and executes one SQL statement, returning its
    /// result set.
    pub fn execute(&mut self, sql: &str) -> Result<ResultSet> {
        let _ = sql;
        Err(Error::NotSupported("the SQL execution pipeline is not wired up yet".to_string()))
    }
}
