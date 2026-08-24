use thiserror::Error;

#[derive(Debug, Error)]
pub enum ExecutorError {
    #[error("expression evaluation error: {0}")]
    Evaluation(String),

    #[error("storage error: {0}")]
    Storage(#[from] storage::StorageError),

    #[error("transaction error: {0}")]
    Transaction(String),

    #[error("catalog error: {0}")]
    Catalog(String),
}

impl From<ExecutorError> for common::Error {
    fn from(err: ExecutorError) -> Self {
        common::Error::Execution(err.to_string())
    }
}
