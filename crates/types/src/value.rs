use std::cmp::Ordering;

use crate::DataType;

/// A runtime value: one cell of a tuple. Every SQL literal, column value,
/// and expression result is a `Value`. `Value::Null` is the single
/// representation of SQL `NULL` and is valid regardless of the column's
/// declared `DataType`.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// SQL `NULL`: the absence of a value, distinct from any typed value.
    Null,
    /// A boolean value.
    Boolean(bool),
    /// A 4-byte signed integer value.
    Integer(i32),
    /// An 8-byte signed integer value.
    BigInt(i64),
    /// An 8-byte floating point value.
    Double(f64),
    /// A UTF-8 string value.
    Varchar(String),
}

impl Value {
    /// The `DataType` this value would occupy, or `None` for `Value::Null`,
    /// which has no type of its own until unified with a column or another
    /// operand's type.
    pub fn data_type(&self) -> Option<DataType> {
        todo!("map each non-null variant to its DataType")
    }

    /// Whether this value is `Value::Null`.
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    /// Three-valued-logic comparison against another value, following SQL
    /// comparison semantics: comparisons involving `NULL` yield `None`
    /// rather than a definite ordering, and comparing across incompatible
    /// types is an error.
    pub fn compare(&self, _other: &Value) -> Result<Option<Ordering>, ValueError> {
        todo!("implement SQL three-valued comparison with type coercion")
    }
}

/// Errors raised while operating on `Value`s, such as comparing or
/// coercing across incompatible types.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ValueError {
    /// The two operands' types cannot be compared or coerced to a common
    /// type.
    #[error("type mismatch: cannot compare {lhs:?} with {rhs:?}")]
    TypeMismatch {
        /// The left-hand operand's type name.
        lhs: String,
        /// The right-hand operand's type name.
        rhs: String,
    },
}
