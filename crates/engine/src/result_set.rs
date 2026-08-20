use types::Tuple;

/// The result of executing one SQL statement: the output column names and
/// the rows produced. A statement with no output (e.g. `INSERT`, `CREATE
/// TABLE`) yields an empty `ResultSet` rather than an error.
#[derive(Debug, Clone, PartialEq)]
pub struct ResultSet {
    columns: Vec<String>,
    rows: Vec<Tuple>,
}

impl ResultSet {
    /// Builds a result set from its output column names and rows.
    pub fn new(columns: Vec<String>, rows: Vec<Tuple>) -> Self {
        Self { columns, rows }
    }

    /// The output column names, in projection order.
    pub fn columns(&self) -> &[String] {
        &self.columns
    }

    /// The result rows, in the order they were produced.
    pub fn rows(&self) -> &[Tuple] {
        &self.rows
    }
}
