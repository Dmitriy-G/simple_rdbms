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
        match err {
            PlannerError::UnknownTable(name) => common::Error::UndefinedTable { name },
            PlannerError::UnknownColumn(name) => common::Error::UndefinedColumn { name },
            PlannerError::ColumnCountMismatch { expected, found } => {
                common::Error::ColumnCountMismatch { expected, found }
            }
            PlannerError::TypeMismatch(detail) => common::Error::DatatypeMismatch { detail },
            PlannerError::AmbiguousColumn(name) => common::Error::AmbiguousColumn { name },
            PlannerError::LiteralOutOfRange { column, value, data_type } => {
                common::Error::NumericValueOutOfRange {
                    column,
                    value,
                    data_type: format!("{data_type:?}"),
                }
            }
        }
    }
}
