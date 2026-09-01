use catalog::{Catalog, Schema};
use common::TableId;
use types::{DataType, Value};

use crate::error::PlannerError;

#[derive(Debug, Clone)]
pub enum BoundStatement {
    Select(BoundSelect),
    Insert(BoundInsert),
    CreateTable(BoundCreateTable),
    CreateIndex(BoundCreateIndex),
    Explain { verbose: bool, inner: Box<BoundStatement> },
}

#[derive(Debug, Clone)]
pub struct BoundSelect {
    pub table_id: TableId,
    pub projections: Vec<BoundExpr>,
    pub column_names: Vec<String>,
    pub predicate: Option<BoundExpr>,
}

#[derive(Debug, Clone)]
pub struct BoundInsert {
    pub table_id: TableId,
    pub rows: Vec<Vec<BoundExpr>>,
}

#[derive(Debug, Clone)]
pub struct BoundCreateTable {
    pub table_name: String,
    pub columns: Vec<BoundColumnDef>,
}

#[derive(Debug, Clone)]
pub struct BoundCreateIndex {
    pub index_name: String,
    pub table_id: TableId,
    pub column_index: usize,
}

#[derive(Debug, Clone)]
pub struct BoundColumnDef {
    pub name: String,
    pub data_type: DataType,
    pub nullable: bool,
}

#[derive(Debug, Clone)]
pub enum BoundExpr {
    Literal(Value),
    ColumnRef {
        index: usize,
        data_type: DataType,
    },
    BinaryOp {
        left: Box<BoundExpr>,
        op: sql::BinaryOperator,
        right: Box<BoundExpr>,
        data_type: DataType,
    },
    UnaryOp {
        op: sql::UnaryOperator,
        expr: Box<BoundExpr>,
        data_type: DataType,
    },
    IsNull {
        expr: Box<BoundExpr>,
        negated: bool,
    },
}

pub struct Binder<'a> {
    catalog: &'a Catalog,
}

impl<'a> Binder<'a> {
    pub fn new(catalog: &'a Catalog) -> Self {
        Self { catalog }
    }

    pub fn bind(&self, statement: sql::Statement) -> Result<BoundStatement, PlannerError> {
        match statement {
            sql::Statement::Select(select) => Ok(BoundStatement::Select(self.bind_select(select)?)),
            sql::Statement::Insert(insert) => Ok(BoundStatement::Insert(self.bind_insert(insert)?)),
            sql::Statement::CreateTable(create) => {
                Ok(BoundStatement::CreateTable(self.bind_create_table(create)))
            }
            sql::Statement::CreateIndex(create) => {
                Ok(BoundStatement::CreateIndex(self.bind_create_index(create)?))
            }
            sql::Statement::Explain { verbose, inner } => {
                let inner = self.bind(*inner)?;
                Ok(BoundStatement::Explain { verbose, inner: Box::new(inner) })
            }
            sql::Statement::Begin | sql::Statement::Commit | sql::Statement::Rollback => {
                unreachable!(
                    "transaction control statements reference no tables or columns and are \
                     intercepted in Database::execute before binding, not bound here"
                )
            }
        }
    }

    fn bind_select(&self, select: sql::SelectStatement) -> Result<BoundSelect, PlannerError> {
        let table = self
            .catalog
            .get_table(&select.from.name)
            .map_err(|_| PlannerError::UnknownTable(select.from.name.clone()))?;
        let table_id = table.table_id;
        let schema = &table.schema;
        let table_scope = select.from.alias.as_deref().unwrap_or(select.from.name.as_str());

        let mut projections = Vec::new();
        let mut column_names = Vec::new();
        for item in &select.items {
            match item {
                sql::SelectItem::Wildcard => {
                    for (index, column) in schema.columns().iter().enumerate() {
                        projections
                            .push(BoundExpr::ColumnRef { index, data_type: column.data_type });
                        column_names.push(column.name.clone());
                    }
                }
                sql::SelectItem::Expr(expr) => {
                    let (bound, _) = self.bind_expr(expr, schema, Some(table_scope))?;
                    column_names.push(match expr {
                        sql::Expr::Column { name, .. } => name.clone(),
                        _ => format!("column{}", column_names.len() + 1),
                    });
                    projections.push(bound);
                }
            }
        }

        let predicate = match &select.where_clause {
            Some(expr) => {
                let (bound, data_type) = self.bind_expr(expr, schema, Some(table_scope))?;
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

        Ok(BoundSelect { table_id, projections, column_names, predicate })
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
                let (bound, data_type) = self.bind_expr(expr, &empty_schema, None)?;
                let (bound, data_type) = match bound {
                    BoundExpr::Literal(value) => {
                        let coerced =
                            coerce_literal_to_column(value, column.data_type, &column.name)?;
                        let data_type = coerced.data_type();
                        (BoundExpr::Literal(coerced), data_type)
                    }
                    other => (other, data_type),
                };
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

    fn bind_create_index(
        &self,
        create: sql::CreateIndexStatement,
    ) -> Result<BoundCreateIndex, PlannerError> {
        let table = self
            .catalog
            .get_table(&create.table)
            .map_err(|_| PlannerError::UnknownTable(create.table.clone()))?;
        let column_index = table
            .schema
            .column_index(&create.column)
            .ok_or_else(|| PlannerError::UnknownColumn(create.column.clone()))?;
        Ok(BoundCreateIndex {
            index_name: create.index_name,
            table_id: table.table_id,
            column_index,
        })
    }

    fn bind_expr(
        &self,
        expr: &sql::Expr,
        schema: &Schema,
        table_scope: Option<&str>,
    ) -> Result<(BoundExpr, Option<DataType>), PlannerError> {
        match expr {
            sql::Expr::Literal(value) => Ok((BoundExpr::Literal(value.clone()), value.data_type())),
            sql::Expr::Column { table, name } => {
                if let Some(qualifier) = table {
                    if table_scope != Some(qualifier.as_str()) {
                        return Err(PlannerError::UnknownTable(qualifier.clone()));
                    }
                }
                let index = schema
                    .column_index(name)
                    .ok_or_else(|| PlannerError::UnknownColumn(name.clone()))?;
                let data_type = schema.columns()[index].data_type;
                Ok((BoundExpr::ColumnRef { index, data_type }, Some(data_type)))
            }
            sql::Expr::UnaryOp { op, expr } => self.bind_unary_op(*op, expr, schema, table_scope),
            sql::Expr::BinaryOp { left, op, right } => {
                self.bind_binary_op(left, *op, right, schema, table_scope)
            }
            sql::Expr::IsNull { expr, negated } => {
                let (bound, _operand_type) = self.bind_expr(expr, schema, table_scope)?;
                let bound = BoundExpr::IsNull { expr: Box::new(bound), negated: *negated };
                Ok((bound, Some(DataType::Boolean)))
            }
        }
    }

    fn bind_unary_op(
        &self,
        op: sql::UnaryOperator,
        expr: &sql::Expr,
        schema: &Schema,
        table_scope: Option<&str>,
    ) -> Result<(BoundExpr, Option<DataType>), PlannerError> {
        let (bound, operand_type) = self.bind_expr(expr, schema, table_scope)?;
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
        table_scope: Option<&str>,
    ) -> Result<(BoundExpr, Option<DataType>), PlannerError> {
        use sql::BinaryOperator::*;

        let (left_bound, left_type) = self.bind_expr(left, schema, table_scope)?;
        let (right_bound, right_type) = self.bind_expr(right, schema, table_scope)?;
        let (left_bound, left_type, right_bound, right_type) =
            coerce_comparison_operands(left_bound, left_type, right_bound, right_type, schema)?;

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

fn is_numeric(data_type: DataType) -> bool {
    matches!(data_type, DataType::Integer | DataType::BigInt | DataType::Double)
}

fn data_types_match(a: DataType, b: DataType) -> bool {
    match (a, b) {
        (DataType::Varchar(_), DataType::Varchar(_)) => true,
        (a, b) => a == b,
    }
}

fn coerce_literal_to_column(
    value: Value,
    target: DataType,
    column_name: &str,
) -> Result<Value, PlannerError> {
    match (&value, target) {
        (Value::BigInt(v), DataType::Integer) => {
            i32::try_from(*v).map(Value::Integer).map_err(|_| PlannerError::LiteralOutOfRange {
                column: column_name.to_string(),
                value: v.to_string(),
                data_type: target,
            })
        }
        _ => Ok(value),
    }
}

fn coerce_comparison_operands(
    left: BoundExpr,
    left_type: Option<DataType>,
    right: BoundExpr,
    right_type: Option<DataType>,
    schema: &Schema,
) -> Result<(BoundExpr, Option<DataType>, BoundExpr, Option<DataType>), PlannerError> {
    match (left, right) {
        (BoundExpr::Literal(v), BoundExpr::ColumnRef { index, data_type: col_type }) => {
            let column_name = &schema.columns()[index].name;
            let coerced = coerce_literal_to_column(v, col_type, column_name)?;
            let coerced_type = coerced.data_type();
            let right = BoundExpr::ColumnRef { index, data_type: col_type };
            Ok((BoundExpr::Literal(coerced), coerced_type, right, right_type))
        }
        (BoundExpr::ColumnRef { index, data_type: col_type }, BoundExpr::Literal(v)) => {
            let column_name = &schema.columns()[index].name;
            let coerced = coerce_literal_to_column(v, col_type, column_name)?;
            let coerced_type = coerced.data_type();
            let left = BoundExpr::ColumnRef { index, data_type: col_type };
            Ok((left, left_type, BoundExpr::Literal(coerced), coerced_type))
        }
        (left, right) => Ok((left, left_type, right, right_type)),
    }
}
