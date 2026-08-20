//! The database facade: the single entry point that wires the SQL, planner,
//! executor, catalog, storage, and transaction crates together behind one
//! `Database::execute(sql) -> ResultSet` call.
//!
//! Every other crate in the workspace is a layer; `engine` is where the
//! layers are assembled. Nothing below this crate knows about any other
//! sibling (`sql` does not know about `executor`, `catalog` does not know
//! about `planner`); `engine` is the only place that does.

#![forbid(unsafe_code)]

mod database;
mod result_set;

pub use database::Database;
pub use result_set::ResultSet;
