use thiserror::Error;

/// The single error type threaded through every layer of the engine.
///
/// Each downstream crate defines its own, more specific error enum for the
/// failures it can produce, and converts that local error into a variant
/// here (typically via `impl From<LocalError> for Error`) at the point where
/// it crosses into a caller from another crate. Keeping one top-level error
/// type means the `engine` facade and the `cli` REPL only ever need to match
/// on one shape.
#[derive(Debug, Error)]
pub enum Error {
    /// A filesystem or OS-level I/O failure (opening/reading/writing the
    /// database file, WAL segments, etc.).
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// A failure inside the storage engine: disk manager, buffer pool,
    /// page layout, heap file, B+tree, or WAL.
    #[error("storage error: {0}")]
    Storage(String),

    /// A failure while lexing or parsing SQL text into an AST.
    #[error("parse error: {0}")]
    Parse(String),

    /// A failure while binding a parsed AST against the catalog (unknown
    /// table/column, type mismatch, ambiguous reference, etc.).
    #[error("binder error: {0}")]
    Binder(String),

    /// A failure raised by the query executor while pulling tuples through
    /// an operator tree.
    #[error("execution error: {0}")]
    Execution(String),

    /// A failure in the transaction subsystem: lock manager, MVCC, or
    /// transaction lifecycle management.
    #[error("transaction error: {0}")]
    Transaction(String),

    /// A failure in the system catalog: table/column already exists,
    /// unknown table, schema mismatch, etc.
    #[error("catalog error: {0}")]
    Catalog(String),

    /// The request is well-formed but exercises functionality that does not
    /// exist yet. Distinguished from the other variants so callers (and
    /// tests) can assert "not implemented" without it being confused for a
    /// real failure.
    #[error("not supported: {0}")]
    NotSupported(String),
}

/// Convenience alias used throughout the workspace instead of
/// `std::result::Result<T, common::Error>`.
pub type Result<T> = std::result::Result<T, Error>;
