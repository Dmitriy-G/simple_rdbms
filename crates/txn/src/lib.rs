//! The transaction subsystem: transaction lifecycle, two-phase locking, and
//! the multi-version concurrency control types used for snapshot
//! isolation.
//!
//! `txn` depends on `storage` (a lock is taken on a physical `Rid`/`PageId`,
//! and MVCC version chains live alongside the tuples they version) but not
//! on `catalog` or `sql`: it has no notion of tables or SQL, only of
//! transactions and the physical records they touch.

#![forbid(unsafe_code)]

mod error;
mod isolation;
mod lock_manager;
mod manager;
mod mvcc;
mod transaction;

pub use error::TxnError;
pub use isolation::IsolationLevel;
pub use lock_manager::{LockManager, LockMode};
pub use manager::TransactionManager;
pub use mvcc::{VersionChain, VersionEntry};
pub use transaction::{Transaction, TransactionState};
