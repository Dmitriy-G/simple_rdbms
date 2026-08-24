use thiserror::Error;
use types::DataType;

#[derive(Debug, Error)]
pub enum PlannerError {
    #[error("unknown table: {0}")]
    UnknownTable(String),

    #[error("unknown column: {0}")]
    UnknownColumn(String),

    #[error("column count mismatch: expected {expected}, found {found}")]
    ColumnCountMismatch { expected: usize, found: usize },

    #[error("type mismatch in expression: {0}")]
    TypeMismatch(String),

    #[error("ambiguous column reference: {0}")]
    AmbiguousColumn(String),

    #[error("value {value} out of range for column {column} ({data_type:?})")]
    LiteralOutOfRange { column: String, value: String, data_type: DataType },
}

impl From<PlannerError> for common::Error {
    fn from(err: PlannerError) -> Self {
        common::Error::Binder(err.to_string())
    }
}
