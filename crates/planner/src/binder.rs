use catalog::Catalog;
use common::TableId;
use types::{DataType, Value};

use crate::error::PlannerError;

/// A statement after binding: names resolved against the catalog and
/// expressions type-checked, but still shaped like the SQL it came from
/// rather than lowered to relational algebra.
#[derive(Debug, Clone)]
pub enum BoundStatement {
    /// A bound `SELECT`.
    Select(BoundSelect),
    /// A bound `INSERT`.
    Insert(BoundInsert),
    /// A bound `CREATE TABLE`.
    CreateTable(BoundCreateTable),
}

/// A `SELECT` after binding.
#[derive(Debug, Clone)]
pub struct BoundSelect {
    /// The resolved source table.
    pub table_id: TableId,
    /// The projected expressions, resolved to column ordinals.
    pub projections: Vec<BoundExpr>,
    /// The optional, type-checked filter predicate.
    pub predicate: Option<BoundExpr>,
}

/// An `INSERT` after binding.
#[derive(Debug, Clone)]
pub struct BoundInsert {
    /// The resolved destination table.
    pub table_id: TableId,
    /// Each row's values, one bound expression per target column, aligned
    /// and coerced to the column's declared type.
    pub rows: Vec<Vec<BoundExpr>>,
}

/// A `CREATE TABLE` after binding (mainly a name-collision check; there is
/// no schema to resolve against yet since the table doesn't exist).
#[derive(Debug, Clone)]
pub struct BoundCreateTable {
    /// The new table's name.
    pub table_name: String,
    /// The new table's column definitions.
    pub columns: Vec<BoundColumnDef>,
}

/// A column definition after binding, e.g. after validating that its type
/// is supported and its name does not repeat within the same statement.
#[derive(Debug, Clone)]
pub struct BoundColumnDef {
    /// The column's name.
    pub name: String,
    /// The column's declared type.
    pub data_type: DataType,
    /// Whether the column allows `NULL`.
    pub nullable: bool,
}

/// A scalar expression after binding: column references are resolved to
/// ordinal positions and literal/operator types have been checked.
#[derive(Debug, Clone)]
pub enum BoundExpr {
    /// A literal value.
    Literal(Value),
    /// A reference to the column at this ordinal position in the bound
    /// source's schema.
    ColumnRef {
        /// The column's ordinal position in the source schema.
        index: usize,
        /// The column's resolved type, cached to avoid re-deriving it
        /// during execution.
        data_type: DataType,
    },
}

/// Binds parsed `sql` AST nodes against a `Catalog`, producing a
/// `BoundStatement` with every name resolved and every expression
/// type-checked. This is the boundary where "syntactically valid SQL"
/// becomes "semantically valid against this database's schema".
pub struct Binder<'a> {
    #[allow(dead_code)]
    catalog: &'a Catalog,
}

impl<'a> Binder<'a> {
    /// Creates a binder that resolves names against `catalog`.
    pub fn new(catalog: &'a Catalog) -> Self {
        Self { catalog }
    }

    /// Binds a parsed statement, resolving every table/column reference and
    /// type-checking every expression.
    pub fn bind(&self, statement: sql::Statement) -> Result<BoundStatement, PlannerError> {
        let _ = statement;
        todo!("dispatch on the statement kind to bind_select/bind_insert/bind_create_table")
    }
}
