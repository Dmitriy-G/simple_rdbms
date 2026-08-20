//! The system catalog: table and column metadata. Tracks the schema of
//! every table and where its heap file begins, so the planner and executor
//! can resolve names to physical storage locations without touching disk.
//!
//! Held in memory only for now; persisting the catalog to disk (as a
//! regular table in its own right, the usual approach) is future work.

#![forbid(unsafe_code)]

mod catalog;
mod column;
mod error;
mod persist;
mod schema;
mod table_info;

pub use catalog::Catalog;
pub use column::Column;
pub use error::CatalogError;
pub use schema::Schema;
pub use table_info::TableInfo;
