use thiserror::Error;
use types::DataType;

/// Errors raised while binding a parsed statement against the catalog.
#[derive(Debug, Error)]
pub enum PlannerError {
    /// A statement referenced a table not present in the catalog.
    #[error("unknown table: {0}")]
    UnknownTable(String),

    /// A statement referenced a column not present in the resolved table's
    /// schema.
    #[error("unknown column: {0}")]
    UnknownColumn(String),

    /// An `INSERT` row supplied a different number of values than the
    /// number of target columns (explicit or, if omitted, the table's
    /// full column list).
    #[error("column count mismatch: expected {expected}, found {found}")]
    ColumnCountMismatch {
        /// The number of target columns.
        expected: usize,
        /// The number of values actually supplied.
        found: usize,
    },

    /// An expression's operand types are incompatible with the operator
    /// applied to them.
    #[error("type mismatch in expression: {0}")]
    TypeMismatch(String),

    /// A column reference could be resolved against more than one source in
    /// scope, and no qualifier was given to disambiguate it.
    #[error("ambiguous column reference: {0}")]
    AmbiguousColumn(String),

    /// A literal being coerced to a column's narrower type does not fit in
    /// it (e.g. a `BigInt` literal too large for an `Integer` column).
    #[error("value {value} out of range for column {column} ({data_type:?})")]
    LiteralOutOfRange {
        /// The name of the column the literal was bound against.
        column: String,
        /// The literal's original text, as written in the source SQL.
        value: String,
        /// The column's declared type the literal did not fit in.
        data_type: DataType,
    },
}

impl From<PlannerError> for common::Error {
    fn from(err: PlannerError) -> Self {
        common::Error::Binder(err.to_string())
    }
}
