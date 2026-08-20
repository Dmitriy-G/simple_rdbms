/// The set of column types the engine understands. A `catalog::Column`
/// carries one of these plus a nullability flag; nullability itself is not
/// part of the type (a `NULL` is a `Value::Null` that can appear in any
/// nullable column, not a distinct `DataType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DataType {
    /// A single-byte boolean, `true` or `false`.
    Boolean,
    /// A 4-byte signed integer.
    Integer,
    /// An 8-byte signed integer.
    BigInt,
    /// An 8-byte IEEE-754 floating point number.
    Double,
    /// A UTF-8 string with an inclusive maximum length in bytes, mirroring
    /// SQL `VARCHAR(n)`.
    Varchar(u32),
}

impl DataType {
    /// The on-disk width in bytes of a fixed-size type, or `None` for a
    /// variable-length type like `Varchar`, which is stored out-of-line
    /// with a length prefix.
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
