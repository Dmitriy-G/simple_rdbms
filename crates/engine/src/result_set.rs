use types::{DataType, Tuple};

#[derive(Debug, Clone, PartialEq)]
pub enum ResultSet {
    Rows { columns: Vec<String>, column_types: Vec<Option<DataType>>, rows: Vec<Tuple> },
    RowsAffected(usize),
    RolledBack,
}

impl ResultSet {
    pub fn rows(
        columns: Vec<String>,
        column_types: Vec<Option<DataType>>,
        rows: Vec<Tuple>,
    ) -> Self {
        ResultSet::Rows { columns, column_types, rows }
    }

    pub fn rows_affected(count: usize) -> Self {
        ResultSet::RowsAffected(count)
    }
}
