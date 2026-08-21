use types::Tuple;

/// The result of executing one SQL statement. A query (`SELECT`) yields
/// `Rows`; a statement with no output rows of its own (`INSERT`, `CREATE
/// TABLE`) yields `RowsAffected` instead of an empty `Rows`, so a caller can
/// tell "no columns because this wasn't a query" apart from "a query that
/// matched nothing".
#[derive(Debug, Clone, PartialEq)]
pub enum ResultSet {
    /// A query's output: column names in projection order, and the rows
    /// produced, in the order they were produced.
    Rows {
        /// The output column names, in projection order.
        columns: Vec<String>,
        /// The result rows, in the order they were produced.
        rows: Vec<Tuple>,
    },
    /// A statement with no output rows: the number of rows it inserted,
    /// updated, or deleted (`0` for DDL like `CREATE TABLE`).
    RowsAffected(usize),
}

impl ResultSet {
    /// Builds a `Rows` result set from its output column names and rows.
    pub fn rows(columns: Vec<String>, rows: Vec<Tuple>) -> Self {
        ResultSet::Rows { columns, rows }
    }

    /// Builds a `RowsAffected` result set.
    pub fn rows_affected(count: usize) -> Self {
        ResultSet::RowsAffected(count)
    }
}
