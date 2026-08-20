use types::DataType;

/// The definition of a single table column: its name, declared type, and
/// whether `NULL` is a legal value for it.
#[derive(Debug, Clone, PartialEq)]
pub struct Column {
    /// The column's name, unique within its table.
    pub name: String,
    /// The column's declared SQL type.
    pub data_type: DataType,
    /// Whether `NULL` is a legal value for this column.
    pub nullable: bool,
}

impl Column {
    /// Builds a column definition.
    pub fn new(name: impl Into<String>, data_type: DataType, nullable: bool) -> Self {
        Self { name: name.into(), data_type, nullable }
    }
}
