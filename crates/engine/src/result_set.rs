use types::Tuple;

#[derive(Debug, Clone, PartialEq)]
pub enum ResultSet {
    Rows { columns: Vec<String>, rows: Vec<Tuple> },
    RowsAffected(usize),
    RolledBack,
}

impl ResultSet {
    pub fn rows(columns: Vec<String>, rows: Vec<Tuple>) -> Self {
        ResultSet::Rows { columns, rows }
    }

    pub fn rows_affected(count: usize) -> Self {
        ResultSet::RowsAffected(count)
    }
}
