use thiserror::Error;

/// Errors raised while executing a physical plan.
#[derive(Debug, Error)]
pub enum ExecutorError {
    /// Evaluating an expression against a tuple failed, e.g. a type
    /// mismatch that the binder should have caught but didn't.
    #[error("expression evaluation error: {0}")]
    Evaluation(String),

    /// The underlying storage layer reported an error while an operator
    /// was reading or writing tuples.
    #[error("storage error: {0}")]
    Storage(#[from] storage::StorageError),

    /// The transaction layer reported an error, e.g. a lock could not be
    /// acquired.
    #[error("transaction error: {0}")]
    Transaction(String),

    /// The catalog reported an error while an operator looked up a table's
    /// metadata, e.g. a table id that no longer resolves.
    #[error("catalog error: {0}")]
    Catalog(String),
}

impl From<ExecutorError> for common::Error {
    fn from(err: ExecutorError) -> Self {
        common::Error::Execution(err.to_string())
    }
}
