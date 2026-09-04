use thiserror::Error;

#[derive(Debug, Error)]
pub enum ExecutorError {
    #[error("expression evaluation error: {0}")]
    Evaluation(String),

    #[error("corrupt tuple: {0}")]
    CorruptTuple(String),

    #[error("not supported: {0}")]
    NotSupported(String),

    #[error("storage error: {0}")]
    Storage(#[from] storage::StorageError),

    #[error("catalog error: {0}")]
    Catalog(#[from] catalog::CatalogError),

    #[error("lock error: {0}")]
    Lock(#[from] txn::TxnError),
}

impl From<ExecutorError> for common::Error {
    fn from(err: ExecutorError) -> Self {
        match err {
            ExecutorError::Evaluation(detail) => common::Error::DatatypeMismatch { detail },
            ExecutorError::CorruptTuple(detail) => common::Error::DataCorrupted { detail },
            ExecutorError::NotSupported(detail) => common::Error::NotSupported(detail),
            ExecutorError::Storage(err) => err.into(),
            ExecutorError::Catalog(err) => err.into(),
            ExecutorError::Lock(err) => err.into(),
        }
    }
}
