use catalog::{Catalog, Schema};
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
    /// Each row's values, one bound expression per column of the target
    /// table's schema (in schema order); columns not named in the
    /// statement's optional column list are bound to `Value::Null`.
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
    /// A binary operator application, resolved and type-checked.
    BinaryOp {
        /// The left-hand operand.
        left: Box<BoundExpr>,
        /// The operator being applied.
        op: sql::BinaryOperator,
        /// The right-hand operand.
        right: Box<BoundExpr>,
        /// The expression's resolved result type.
        data_type: DataType,
    },
    /// A unary operator application, resolved and type-checked.
    UnaryOp {
        /// The operator being applied.
        op: sql::UnaryOperator,
        /// The operand.
        expr: Box<BoundExpr>,
        /// The expression's resolved result type.
        data_type: DataType,
    },
}

/// Binds parsed `sql` AST nodes against a `Catalog`, producing a
/// `BoundStatement` with every name resolved and every expression
/// type-checked. This is the boundary where "syntactically valid SQL"
/// becomes "semantically valid against this database's schema".
pub struct Binder<'a> {
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
        match statement {
            sql::Statement::Select(select) => Ok(BoundStatement::Select(self.bind_select(select)?)),
            sql::Statement::Insert(insert) => Ok(BoundStatement::Insert(self.bind_insert(insert)?)),
            sql::Statement::CreateTable(create) => {
                Ok(BoundStatement::CreateTable(self.bind_create_table(create)))
            }
        }
    }

    fn bind_select(&self, select: sql::SelectStatement) -> Result<BoundSelect, PlannerError> {
        let table = self
            .catalog
            .get_table(&select.from)
            .map_err(|_| PlannerError::UnknownTable(select.from.clone()))?;
        let table_id = table.table_id;
        let schema = &table.schema;

        let mut projections = Vec::new();
        for item in &select.items {
            match item {
                sql::SelectItem::Wildcard => {
                    for (index, column) in schema.columns().iter().enumerate() {
                        projections
                            .push(BoundExpr::ColumnRef { index, data_type: column.data_type });
                    }
                }
                sql::SelectItem::Expr(expr) => {
                    let (bound, _) = self.bind_expr(expr, schema)?;
                    projections.push(bound);
                }
            }
        }

        let predicate = match &select.where_clause {
            Some(expr) => {
                let (bound, data_type) = self.bind_expr(expr, schema)?;
                if let Some(data_type) = data_type {
                    if data_type != DataType::Boolean {
                        return Err(PlannerError::TypeMismatch(format!(
                            "WHERE clause must be Boolean, found {data_type:?}"
                        )));
                    }
                }
                Some(bound)
            }
            None => None,
        };

        Ok(BoundSelect { table_id, projections, predicate })
    }

    fn bind_insert(&self, insert: sql::InsertStatement) -> Result<BoundInsert, PlannerError> {
        let table = self
            .catalog
            .get_table(&insert.table)
            .map_err(|_| PlannerError::UnknownTable(insert.table.clone()))?;
        let table_id = table.table_id;
        let schema = &table.schema;

        let target_indices: Vec<usize> = if insert.columns.is_empty() {
            (0..schema.columns().len()).collect()
        } else {
            insert
                .columns
                .iter()
                .map(|name| {
                    schema
                        .column_index(name)
                        .ok_or_else(|| PlannerError::UnknownColumn(name.clone()))
                })
                .collect::<Result<_, _>>()?
        };

        // Values in an INSERT have no source row to resolve column
        // references against; binding against an empty schema turns any
        // bare column reference into an `UnknownColumn` error.
        let empty_schema = Schema::new(Vec::new());

        let mut rows = Vec::with_capacity(insert.values.len());
        for row in &insert.values {
            if row.len() != target_indices.len() {
                return Err(PlannerError::ColumnCountMismatch {
                    expected: target_indices.len(),
                    found: row.len(),
                });
            }

            let mut bound_row: Vec<BoundExpr> =
                (0..schema.columns().len()).map(|_| BoundExpr::Literal(Value::Null)).collect();
            for (expr, &col_index) in row.iter().zip(target_indices.iter()) {
                let column = &schema.columns()[col_index];
                let (bound, data_type) = self.bind_expr(expr, &empty_schema)?;
                if let Some(data_type) = data_type {
                    if !data_types_match(data_type, column.data_type) {
                        return Err(PlannerError::TypeMismatch(format!(
                            "column {} expects {:?}, found {data_type:?}",
                            column.name, column.data_type
                        )));
                    }
                }
                bound_row[col_index] = bound;
            }
            rows.push(bound_row);
        }

        Ok(BoundInsert { table_id, rows })
    }

    fn bind_create_table(&self, create: sql::CreateTableStatement) -> BoundCreateTable {
        let columns = create
            .columns
            .into_iter()
            .map(|column| BoundColumnDef {
                name: column.name,
                data_type: column.data_type,
                nullable: column.nullable,
            })
            .collect();
        BoundCreateTable { table_name: create.table, columns }
    }

    /// Binds an expression against `schema`, resolving column references to
    /// ordinals and type-checking every operator application. Returns the
    /// bound expression along with its resolved type, or `None` if the
    /// expression is a `NULL` literal (which has no type of its own and is
    /// treated as compatible with any type it's checked against).
    fn bind_expr(
        &self,
        expr: &sql::Expr,
        schema: &Schema,
    ) -> Result<(BoundExpr, Option<DataType>), PlannerError> {
        match expr {
            sql::Expr::Literal(value) => Ok((BoundExpr::Literal(value.clone()), value.data_type())),
            sql::Expr::Column(name) => {
                let index = schema
                    .column_index(name)
                    .ok_or_else(|| PlannerError::UnknownColumn(name.clone()))?;
                let data_type = schema.columns()[index].data_type;
                Ok((BoundExpr::ColumnRef { index, data_type }, Some(data_type)))
            }
            sql::Expr::UnaryOp { op, expr } => self.bind_unary_op(*op, expr, schema),
            sql::Expr::BinaryOp { left, op, right } => {
                self.bind_binary_op(left, *op, right, schema)
            }
        }
    }

    fn bind_unary_op(
        &self,
        op: sql::UnaryOperator,
        expr: &sql::Expr,
        schema: &Schema,
    ) -> Result<(BoundExpr, Option<DataType>), PlannerError> {
        let (bound, operand_type) = self.bind_expr(expr, schema)?;
        match op {
            sql::UnaryOperator::Not => {
                if let Some(data_type) = operand_type {
                    if data_type != DataType::Boolean {
                        return Err(PlannerError::TypeMismatch(format!(
                            "NOT requires a Boolean operand, found {data_type:?}"
                        )));
                    }
                }
                let bound =
                    BoundExpr::UnaryOp { op, expr: Box::new(bound), data_type: DataType::Boolean };
                Ok((bound, Some(DataType::Boolean)))
            }
            sql::UnaryOperator::Negate => {
                let data_type = operand_type.ok_or_else(|| {
                    PlannerError::TypeMismatch(
                        "unary - requires a numeric operand, found NULL".to_string(),
                    )
                })?;
                if !is_numeric(data_type) {
                    return Err(PlannerError::TypeMismatch(format!(
                        "unary - requires a numeric operand, found {data_type:?}"
                    )));
                }
                let bound = BoundExpr::UnaryOp { op, expr: Box::new(bound), data_type };
                Ok((bound, Some(data_type)))
            }
        }
    }

    fn bind_binary_op(
        &self,
        left: &sql::Expr,
        op: sql::BinaryOperator,
        right: &sql::Expr,
        schema: &Schema,
    ) -> Result<(BoundExpr, Option<DataType>), PlannerError> {
        use sql::BinaryOperator::*;

        let (left_bound, left_type) = self.bind_expr(left, schema)?;
        let (right_bound, right_type) = self.bind_expr(right, schema)?;

        match op {
            And | Or => {
                for data_type in [left_type, right_type].into_iter().flatten() {
                    if data_type != DataType::Boolean {
                        return Err(PlannerError::TypeMismatch(format!(
                            "{op:?} requires Boolean operands, found {data_type:?}"
                        )));
                    }
                }
                let bound = BoundExpr::BinaryOp {
                    left: Box::new(left_bound),
                    op,
                    right: Box::new(right_bound),
                    data_type: DataType::Boolean,
                };
                Ok((bound, Some(DataType::Boolean)))
            }
            Eq | NotEq | Lt | LtEq | Gt | GtEq => {
                if let (Some(l), Some(r)) = (left_type, right_type) {
                    if !data_types_match(l, r) {
                        return Err(PlannerError::TypeMismatch(format!(
                            "cannot compare {l:?} with {r:?}"
                        )));
                    }
                }
                let bound = BoundExpr::BinaryOp {
                    left: Box::new(left_bound),
                    op,
                    right: Box::new(right_bound),
                    data_type: DataType::Boolean,
                };
                Ok((bound, Some(DataType::Boolean)))
            }
            Plus | Minus | Multiply | Divide => Err(PlannerError::TypeMismatch(
                "arithmetic operators are not supported".to_string(),
            )),
        }
    }
}

/// Whether `data_type` is a numeric type eligible for unary negation.
fn is_numeric(data_type: DataType) -> bool {
    matches!(data_type, DataType::Integer | DataType::BigInt | DataType::Double)
}

/// Whether two resolved types may appear together in a comparison or an
/// assignment, ignoring a `Varchar`'s declared maximum length (an identity
/// property of the column, not of the value being compared against it).
fn data_types_match(a: DataType, b: DataType) -> bool {
    match (a, b) {
        (DataType::Varchar(_), DataType::Varchar(_)) => true,
        (a, b) => a == b,
    }
}
