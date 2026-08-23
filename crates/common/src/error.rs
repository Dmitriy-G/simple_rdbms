use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("storage error: {0}")]
    Storage(String),

    #[error("parse error: {0}")]
    Parse(String),

    #[error("binder error: {0}")]
    Binder(String),

    #[error("execution error: {0}")]
    Execution(String),

    #[error("transaction error: {0}")]
    Transaction(String),

    #[error("catalog error: {0}")]
    Catalog(String),

    #[error("not supported: {0}")]
    NotSupported(String),
}

pub type Result<T> = std::result::Result<T, Error>;
