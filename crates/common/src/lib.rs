//! Cross-cutting types shared by every crate in the workspace: the unified
//! error type, its `Result` alias, newtype identifiers for the core entities
//! that flow between subsystems, and top-level database configuration.
//!
//! `common` sits at the bottom of the dependency graph and must never depend
//! on any other workspace crate.

#![forbid(unsafe_code)]

mod config;
mod error;
mod ids;

pub use config::DbConfig;
pub use error::{Error, Result};
pub use ids::{ColumnId, FrameId, Lsn, PageId, Rid, TableId, TxnId};
