use thiserror::Error;

use crate::sql_state::SqlState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Fatal,
    Panic,
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("checksum mismatch on page {page_id}: expected {expected:#010x}, got {actual:#010x}")]
    ChecksumMismatch { page_id: u32, expected: u32, actual: u32 },

    #[error("corrupt page {page_id}: {reason}")]
    CorruptPage { page_id: u32, reason: String },

    #[error("corrupt write-ahead log: {reason}")]
    CorruptLog { reason: String },

    #[error(
        "database file length {actual} bytes is not a multiple of the {page_size}-byte page \
         size (expected {expected} bytes for a whole number of pages)"
    )]
    TruncatedFile { actual: u64, expected: u64, page_size: usize },

    #[error("page {page_id} not found")]
    PageNotFound { page_id: u32 },

    #[error(
        "double-write restore for page {page_id} did not survive its own write; the batch is \
         left in place for a retry"
    )]
    DoubleWriteRestoreFailed { page_id: u32 },

    #[error("buffer pool exhausted: no evictable frame available")]
    BufferPoolExhausted,

    #[error("timed out after waiting {waited_ms}ms for a free buffer pool frame")]
    BufferPoolWaitTimedOut { waited_ms: u64 },

    #[error("tuple of {size} bytes exceeds the {max}-byte maximum for a single page")]
    TupleTooLarge { size: usize, max: usize },

    #[error("index key of {size} bytes exceeds the {max}-byte maximum a single node can hold")]
    KeyTooLarge { size: usize, max: usize },

    #[error("invalid configuration: {detail}")]
    InvalidConfiguration { detail: String },

    #[error("{message}")]
    Syntax { message: String, offset: usize },

    #[error("column count mismatch: expected {expected}, found {found}")]
    ColumnCountMismatch { expected: usize, found: usize },

    #[error("undefined table: {name}")]
    UndefinedTable { name: String },

    #[error("undefined column: {name}")]
    UndefinedColumn { name: String },

    #[error("ambiguous column reference: {name}")]
    AmbiguousColumn { name: String },

    #[error("table already exists: {name}")]
    DuplicateTable { name: String },

    #[error("undefined index: {name}")]
    UndefinedIndex { name: String },

    #[error("index already exists: {name}")]
    DuplicateIndex { name: String },

    #[error("datatype mismatch: {detail}")]
    DatatypeMismatch { detail: String },

    #[error("value {value} out of range for column {column} ({data_type})")]
    NumericValueOutOfRange { column: String, value: String, data_type: String },

    #[error("data corrupted: {detail}")]
    DataCorrupted { detail: String },

    #[error("serialization failure: {detail}")]
    SerializationFailure { detail: String },

    #[error("internal error: {detail}")]
    Internal { detail: String },

    #[error("already inside a transaction; nested BEGIN is not allowed")]
    NestedTransaction,

    #[error("no active transaction to {statement}")]
    NoActiveTransaction { statement: String },

    #[error("current transaction is aborted; statements are ignored until ROLLBACK")]
    TransactionAborted,

    #[error("not supported: {0}")]
    NotSupported(String),

    #[error("another process has this database open: {path}")]
    DatabaseLocked { path: String },
}

impl Error {
    pub fn sql_state(&self) -> SqlState {
        match self {
            Error::Io(_) => SqlState::IO_ERROR,
            Error::ChecksumMismatch { .. } => SqlState::DATA_CORRUPTED,
            Error::CorruptPage { .. } => SqlState::DATA_CORRUPTED,
            Error::CorruptLog { .. } => SqlState::DATA_CORRUPTED,
            Error::TruncatedFile { .. } => SqlState::DATA_CORRUPTED,
            Error::PageNotFound { .. } => SqlState::DATA_CORRUPTED,
            Error::DoubleWriteRestoreFailed { .. } => SqlState::IO_ERROR,
            Error::BufferPoolExhausted => SqlState::OUT_OF_MEMORY,
            Error::BufferPoolWaitTimedOut { .. } => SqlState::OUT_OF_MEMORY,
            Error::TupleTooLarge { .. } => SqlState::PROGRAM_LIMIT_EXCEEDED,
            Error::KeyTooLarge { .. } => SqlState::PROGRAM_LIMIT_EXCEEDED,
            Error::InvalidConfiguration { .. } => SqlState::CONNECTION_FAILURE,
            Error::Syntax { .. } => SqlState::SYNTAX_ERROR,
            Error::ColumnCountMismatch { .. } => SqlState::SYNTAX_ERROR,
            Error::UndefinedTable { .. } => SqlState::UNDEFINED_TABLE,
            Error::UndefinedColumn { .. } => SqlState::UNDEFINED_COLUMN,
            Error::AmbiguousColumn { .. } => SqlState::AMBIGUOUS_COLUMN,
            Error::DuplicateTable { .. } => SqlState::DUPLICATE_TABLE,
            Error::UndefinedIndex { .. } => SqlState::UNDEFINED_OBJECT,
            Error::DuplicateIndex { .. } => SqlState::DUPLICATE_OBJECT,
            Error::DatatypeMismatch { .. } => SqlState::DATATYPE_MISMATCH,
            Error::NumericValueOutOfRange { .. } => SqlState::NUMERIC_VALUE_OUT_OF_RANGE,
            Error::DataCorrupted { .. } => SqlState::DATA_CORRUPTED,
            Error::SerializationFailure { .. } => SqlState::SERIALIZATION_FAILURE,
            Error::Internal { .. } => SqlState::INTERNAL_ERROR,
            Error::NestedTransaction => SqlState::NO_ACTIVE_SQL_TRANSACTION,
            Error::NoActiveTransaction { .. } => SqlState::NO_ACTIVE_SQL_TRANSACTION,
            Error::TransactionAborted => SqlState::IN_FAILED_SQL_TRANSACTION,
            Error::NotSupported(_) => SqlState::FEATURE_NOT_SUPPORTED,
            Error::DatabaseLocked { .. } => SqlState::LOCK_NOT_AVAILABLE,
        }
    }

    pub fn severity(&self) -> Severity {
        match self {
            Error::Io(_)
            | Error::ChecksumMismatch { .. }
            | Error::CorruptPage { .. }
            | Error::CorruptLog { .. }
            | Error::TruncatedFile { .. }
            | Error::PageNotFound { .. }
            | Error::DoubleWriteRestoreFailed { .. }
            | Error::InvalidConfiguration { .. }
            | Error::DataCorrupted { .. }
            | Error::Internal { .. }
            | Error::DatabaseLocked { .. } => Severity::Fatal,

            Error::BufferPoolExhausted
            | Error::BufferPoolWaitTimedOut { .. }
            | Error::TupleTooLarge { .. }
            | Error::KeyTooLarge { .. }
            | Error::Syntax { .. }
            | Error::ColumnCountMismatch { .. }
            | Error::UndefinedTable { .. }
            | Error::UndefinedColumn { .. }
            | Error::AmbiguousColumn { .. }
            | Error::DuplicateTable { .. }
            | Error::UndefinedIndex { .. }
            | Error::DuplicateIndex { .. }
            | Error::DatatypeMismatch { .. }
            | Error::NumericValueOutOfRange { .. }
            | Error::SerializationFailure { .. }
            | Error::NestedTransaction
            | Error::NoActiveTransaction { .. }
            | Error::TransactionAborted
            | Error::NotSupported(_) => Severity::Error,
        }
    }

    pub fn is_retryable(&self) -> bool {
        matches!(
            self.sql_state(),
            SqlState::SERIALIZATION_FAILURE | SqlState::STATEMENT_COMPLETION_UNKNOWN
        )
    }

    pub fn redacted(&self) -> String {
        match self {
            Error::Io(_)
            | Error::ChecksumMismatch { .. }
            | Error::CorruptPage { .. }
            | Error::CorruptLog { .. }
            | Error::TruncatedFile { .. }
            | Error::PageNotFound { .. }
            | Error::DoubleWriteRestoreFailed { .. }
            | Error::BufferPoolExhausted
            | Error::BufferPoolWaitTimedOut { .. }
            | Error::TupleTooLarge { .. }
            | Error::KeyTooLarge { .. }
            | Error::InvalidConfiguration { .. }
            | Error::ColumnCountMismatch { .. }
            | Error::UndefinedTable { .. }
            | Error::UndefinedColumn { .. }
            | Error::AmbiguousColumn { .. }
            | Error::DuplicateTable { .. }
            | Error::UndefinedIndex { .. }
            | Error::DuplicateIndex { .. }
            | Error::DataCorrupted { .. }
            | Error::SerializationFailure { .. }
            | Error::Internal { .. }
            | Error::NestedTransaction
            | Error::NoActiveTransaction { .. }
            | Error::TransactionAborted
            | Error::NotSupported(_)
            | Error::DatabaseLocked { .. } => self.to_string(),

            Error::Syntax { offset, .. } => format!("syntax error at offset {offset}: ?"),

            Error::DatatypeMismatch { .. } => "datatype mismatch: ?".to_string(),

            Error::NumericValueOutOfRange { column, data_type, .. } => {
                format!("value ? out of range for column {column} ({data_type})")
            }
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;
