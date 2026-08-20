use crate::column::Column;

/// An ordered list of columns describing the shape of a table's tuples.
/// Column order here is the same order values appear in a `types::Tuple`
/// and in the tuple's encoded on-disk bytes.
#[derive(Debug, Clone, PartialEq)]
pub struct Schema {
    columns: Vec<Column>,
}

impl Schema {
    /// Builds a schema from an ordered list of columns.
    pub fn new(columns: Vec<Column>) -> Self {
        Self { columns }
    }

    /// The schema's columns, in tuple order.
    pub fn columns(&self) -> &[Column] {
        &self.columns
    }

    /// The ordinal position of the column named `name`, or `None` if no
    /// such column exists.
    pub fn column_index(&self, name: &str) -> Option<usize> {
        self.columns.iter().position(|column| column.name == name)
    }
}
