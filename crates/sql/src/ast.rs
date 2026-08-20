use types::{DataType, Value};

/// A single parsed SQL statement. The unit of work the `planner`'s binder
/// consumes.
#[derive(Debug, Clone, PartialEq)]
pub enum Statement {
    /// A `SELECT` query.
    Select(SelectStatement),
    /// An `INSERT INTO ... VALUES ...` statement.
    Insert(InsertStatement),
    /// A `CREATE TABLE` statement.
    CreateTable(CreateTableStatement),
}

/// A `SELECT` query: `SELECT <items> FROM <table> [WHERE <expr>]`. No
/// joins, `GROUP BY`, or `ORDER BY` yet.
#[derive(Debug, Clone, PartialEq)]
pub struct SelectStatement {
    /// The projected items, in output order.
    pub items: Vec<SelectItem>,
    /// The single source table's name.
    pub from: String,
    /// The optional filter predicate.
    pub where_clause: Option<Expr>,
}

/// One item in a `SELECT` list.
#[derive(Debug, Clone, PartialEq)]
pub enum SelectItem {
    /// `*`: every column of the source table.
    Wildcard,
    /// A single projected expression.
    Expr(Expr),
}

/// An `INSERT INTO <table> [(<columns>)] VALUES (<row>), ...` statement.
#[derive(Debug, Clone, PartialEq)]
pub struct InsertStatement {
    /// The destination table's name.
    pub table: String,
    /// The target columns, in the order values are given. Empty means "all
    /// columns, in schema order".
    pub columns: Vec<String>,
    /// The rows being inserted, each a list of value expressions aligned
    /// with `columns`.
    pub values: Vec<Vec<Expr>>,
}

/// A `CREATE TABLE <table> (<columns>)` statement.
#[derive(Debug, Clone, PartialEq)]
pub struct CreateTableStatement {
    /// The new table's name.
    pub table: String,
    /// The new table's column definitions, in declared order.
    pub columns: Vec<ColumnDef>,
}

/// One column definition inside a `CREATE TABLE` statement.
#[derive(Debug, Clone, PartialEq)]
pub struct ColumnDef {
    /// The column's name.
    pub name: String,
    /// The column's declared type.
    pub data_type: DataType,
    /// Whether the column allows `NULL` (true unless `NOT NULL` was given).
    pub nullable: bool,
}

/// A scalar SQL expression, as parsed (not yet bound to a schema).
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    /// A literal value.
    Literal(Value),
    /// A bare column reference.
    Column(String),
    /// A binary operator application.
    BinaryOp {
        /// The left-hand operand.
        left: Box<Expr>,
        /// The operator being applied.
        op: BinaryOperator,
        /// The right-hand operand.
        right: Box<Expr>,
    },
    /// A unary operator application.
    UnaryOp {
        /// The operator being applied.
        op: UnaryOperator,
        /// The operand.
        expr: Box<Expr>,
    },
}

/// A binary operator that can appear in an `Expr::BinaryOp`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOperator {
    /// `=`
    Eq,
    /// `<>` / `!=`
    NotEq,
    /// `<`
    Lt,
    /// `<=`
    LtEq,
    /// `>`
    Gt,
    /// `>=`
    GtEq,
    /// `AND`
    And,
    /// `OR`
    Or,
    /// `+`
    Plus,
    /// `-`
    Minus,
    /// `*`
    Multiply,
    /// `/`
    Divide,
}

/// A unary operator that can appear in an `Expr::UnaryOp`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOperator {
    /// `NOT`
    Not,
    /// Unary `-`
    Negate,
}
