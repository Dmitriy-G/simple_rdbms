use thiserror::Error;

/// Errors raised by the system catalog.
#[derive(Debug, Error)]
pub enum CatalogError {
    /// A `CREATE TABLE` named a table that already exists.
    #[error("table already exists: {0}")]
    TableAlreadyExists(String),

    /// A statement referenced a table the catalog has no entry for.
    #[error("table not found: {0}")]
    TableNotFound(String),

    /// A statement referenced a column the table's schema has no entry
    /// for.
    #[error("column not found: {table}.{column}")]
    ColumnNotFound {
        /// The table the column was looked up on.
        table: String,
        /// The missing column's name.
        column: String,
    },

    /// The storage layer failed while reading or writing the catalog's own
    /// persisted table heap.
    #[error("storage error: {0}")]
    Storage(#[from] storage::StorageError),

    /// A persisted catalog row could not be decoded back into a
    /// `TableInfo` (truncated row, invalid schema blob, unknown type tag).
    #[error("corrupt catalog data: {0}")]
    Corrupt(String),
}

impl From<CatalogError> for common::Error {
    fn from(err: CatalogError) -> Self {
        common::Error::Catalog(err.to_string())
    }
}
