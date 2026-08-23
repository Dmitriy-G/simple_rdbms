#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DataType {
    Boolean,
    Integer,
    BigInt,
    Double,
    Varchar(u32),
}

impl DataType {
    pub fn fixed_width(&self) -> Option<usize> {
        match self {
            DataType::Boolean => Some(1),
            DataType::Integer => Some(4),
            DataType::BigInt => Some(8),
            DataType::Double => Some(8),
            DataType::Varchar(_) => None,
        }
    }
}
