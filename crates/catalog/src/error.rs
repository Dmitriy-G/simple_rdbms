use thiserror::Error;

#[derive(Debug, Error)]
pub enum CatalogError {
    #[error("table already exists: {0}")]
    TableAlreadyExists(String),

    #[error("table not found: {0}")]
    TableNotFound(String),

    #[error("column not found: {table}.{column}")]
    ColumnNotFound { table: String, column: String },

    #[error("storage error: {0}")]
    Storage(#[from] storage::StorageError),

    #[error("corrupt catalog data: {0}")]
    Corrupt(String),
}

impl From<CatalogError> for common::Error {
    fn from(err: CatalogError) -> Self {
        match err {
            CatalogError::TableAlreadyExists(name) => common::Error::DuplicateTable { name },
            CatalogError::TableNotFound(name) => common::Error::UndefinedTable { name },
            CatalogError::ColumnNotFound { table, column } => {
                common::Error::UndefinedColumn { name: format!("{table}.{column}") }
            }
            CatalogError::Storage(err) => err.into(),
            CatalogError::Corrupt(detail) => common::Error::DataCorrupted { detail },
        }
    }
}
