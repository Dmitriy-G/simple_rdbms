use std::cmp::Ordering;

use crate::DataType;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Boolean(bool),
    Integer(i32),
    BigInt(i64),
    Double(f64),
    Varchar(String),
}

impl Value {
    pub fn data_type(&self) -> Option<DataType> {
        match self {
            Value::Null => None,
            Value::Boolean(_) => Some(DataType::Boolean),
            Value::Integer(_) => Some(DataType::Integer),
            Value::BigInt(_) => Some(DataType::BigInt),
            Value::Double(_) => Some(DataType::Double),
            Value::Varchar(s) => Some(DataType::Varchar(s.len() as u32)),
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    fn type_name(&self) -> String {
        match self {
            Value::Null => "Null".to_string(),
            Value::Boolean(_) => "Boolean".to_string(),
            Value::Integer(_) => "Integer".to_string(),
            Value::BigInt(_) => "BigInt".to_string(),
            Value::Double(_) => "Double".to_string(),
            Value::Varchar(_) => "Varchar".to_string(),
        }
    }

    pub fn compare(&self, other: &Value) -> Result<Option<Ordering>, ValueError> {
        if self.is_null() || other.is_null() {
            return Ok(None);
        }
        match (self, other) {
            (Value::Boolean(a), Value::Boolean(b)) => Ok(Some(a.cmp(b))),
            (Value::Integer(a), Value::Integer(b)) => Ok(Some(a.cmp(b))),
            (Value::BigInt(a), Value::BigInt(b)) => Ok(Some(a.cmp(b))),
            (Value::Double(a), Value::Double(b)) => Ok(a.partial_cmp(b)),
            (Value::Varchar(a), Value::Varchar(b)) => Ok(Some(a.cmp(b))),
            _ => Err(ValueError::TypeMismatch { lhs: self.type_name(), rhs: other.type_name() }),
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ValueError {
    #[error("type mismatch: cannot compare {lhs:?} with {rhs:?}")]
    TypeMismatch { lhs: String, rhs: String },
}
