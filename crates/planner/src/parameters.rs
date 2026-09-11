use std::collections::BTreeMap;

use catalog::{Catalog, Schema};
use types::{DataType, Value};

use crate::error::PlannerError;

pub fn infer_parameter_types(
    statement: &sql::Statement,
    catalog: &Catalog,
) -> Result<Vec<Option<DataType>>, PlannerError> {
    let mut types: BTreeMap<u32, Option<DataType>> = BTreeMap::new();
    collect_statement(statement, catalog, &mut types)?;
    finalize(types)
}

pub fn substitute_parameters(
    statement: sql::Statement,
    values: &[Value],
) -> Result<sql::Statement, PlannerError> {
    let expected = max_parameter_index(&statement) as usize;
    if expected != values.len() {
        return Err(PlannerError::ParameterCountMismatch { expected, found: values.len() });
    }
    Ok(substitute_statement(statement, values))
}

fn finalize(types: BTreeMap<u32, Option<DataType>>) -> Result<Vec<Option<DataType>>, PlannerError> {
    let Some(&max) = types.keys().max() else {
        return Ok(Vec::new());
    };
    let mut result = Vec::with_capacity(max as usize);
    for index in 1..=max {
        match types.get(&index) {
            Some(data_type) => result.push(*data_type),
            None => return Err(PlannerError::UndefinedParameter { index }),
        }
    }
    Ok(result)
}

fn record(types: &mut BTreeMap<u32, Option<DataType>>, index: u32, data_type: Option<DataType>) {
    let entry = types.entry(index).or_insert(None);
    if entry.is_none() {
        *entry = data_type;
    }
}

fn collect_statement(
    statement: &sql::Statement,
    catalog: &Catalog,
    types: &mut BTreeMap<u32, Option<DataType>>,
) -> Result<(), PlannerError> {
    match statement {
        sql::Statement::Select(select) => collect_select(select, catalog, types),
        sql::Statement::Insert(insert) => collect_insert(insert, catalog, types),
        sql::Statement::CreateTable(_) | sql::Statement::CreateIndex(_) => Ok(()),
        sql::Statement::Explain { inner, .. } => collect_statement(inner, catalog, types),
        sql::Statement::Begin | sql::Statement::Commit | sql::Statement::Rollback => Ok(()),
    }
}

fn collect_select(
    select: &sql::SelectStatement,
    catalog: &Catalog,
    types: &mut BTreeMap<u32, Option<DataType>>,
) -> Result<(), PlannerError> {
    let table = match &select.from {
        Some(from) => Some(
            catalog
                .get_table(&from.name)
                .map_err(|_| PlannerError::UnknownTable(from.name.clone()))?,
        ),
        None => None,
    };
    let schema = table.as_ref().map(|table| &table.schema);
    let table_scope =
        select.from.as_ref().map(|from| from.alias.as_deref().unwrap_or(from.name.as_str()));

    for item in &select.items {
        if let sql::SelectItem::Expr(expr) = item {
            collect_expr(expr, schema, table_scope, types)?;
        }
    }
    if let Some(where_clause) = &select.where_clause {
        collect_expr(where_clause, schema, table_scope, types)?;
    }
    Ok(())
}

fn collect_insert(
    insert: &sql::InsertStatement,
    catalog: &Catalog,
    types: &mut BTreeMap<u32, Option<DataType>>,
) -> Result<(), PlannerError> {
    let table = catalog
        .get_table(&insert.table)
        .map_err(|_| PlannerError::UnknownTable(insert.table.clone()))?;
    let schema = &table.schema;

    let target_indices: Vec<usize> = if insert.columns.is_empty() {
        (0..schema.columns().len()).collect()
    } else {
        insert
            .columns
            .iter()
            .map(|name| {
                schema.column_index(name).ok_or_else(|| PlannerError::UnknownColumn(name.clone()))
            })
            .collect::<Result<_, _>>()?
    };

    for row in &insert.values {
        for (expr, &col_index) in row.iter().zip(target_indices.iter()) {
            match expr {
                sql::Expr::Parameter { index } => {
                    let data_type = schema.columns()[col_index].data_type;
                    record(types, *index, Some(data_type));
                }
                other => collect_expr(other, None, None, types)?,
            }
        }
    }
    Ok(())
}

fn collect_expr(
    expr: &sql::Expr,
    schema: Option<&Schema>,
    table_scope: Option<&str>,
    types: &mut BTreeMap<u32, Option<DataType>>,
) -> Result<(), PlannerError> {
    match expr {
        sql::Expr::Parameter { index } => {
            record(types, *index, None);
            Ok(())
        }
        sql::Expr::Literal(_) | sql::Expr::Column { .. } | sql::Expr::SessionContext(_) => Ok(()),
        sql::Expr::UnaryOp { expr, .. } => collect_expr(expr, schema, table_scope, types),
        sql::Expr::IsNull { expr, .. } => collect_expr(expr, schema, table_scope, types),
        sql::Expr::BinaryOp { left, right, .. } => {
            collect_binary(left, right, schema, table_scope, types)
        }
    }
}

fn collect_binary(
    left: &sql::Expr,
    right: &sql::Expr,
    schema: Option<&Schema>,
    table_scope: Option<&str>,
    types: &mut BTreeMap<u32, Option<DataType>>,
) -> Result<(), PlannerError> {
    if let sql::Expr::Parameter { index } = left {
        record(types, *index, column_type(right, schema, table_scope));
    } else {
        collect_expr(left, schema, table_scope, types)?;
    }
    if let sql::Expr::Parameter { index } = right {
        record(types, *index, column_type(left, schema, table_scope));
    } else {
        collect_expr(right, schema, table_scope, types)?;
    }
    Ok(())
}

fn column_type(
    expr: &sql::Expr,
    schema: Option<&Schema>,
    table_scope: Option<&str>,
) -> Option<DataType> {
    let sql::Expr::Column { table, name } = expr else {
        return None;
    };
    let schema = schema?;
    if let Some(qualifier) = table {
        if table_scope != Some(qualifier.as_str()) {
            return None;
        }
    }
    let index = schema.column_index(name)?;
    Some(schema.columns()[index].data_type)
}

fn max_parameter_index(statement: &sql::Statement) -> u32 {
    let mut max = 0u32;
    scan_statement(statement, &mut max);
    max
}

fn scan_statement(statement: &sql::Statement, max: &mut u32) {
    match statement {
        sql::Statement::Select(select) => {
            for item in &select.items {
                if let sql::SelectItem::Expr(expr) = item {
                    scan_expr(expr, max);
                }
            }
            if let Some(where_clause) = &select.where_clause {
                scan_expr(where_clause, max);
            }
        }
        sql::Statement::Insert(insert) => {
            for row in &insert.values {
                for expr in row {
                    scan_expr(expr, max);
                }
            }
        }
        sql::Statement::CreateTable(_) | sql::Statement::CreateIndex(_) => {}
        sql::Statement::Explain { inner, .. } => scan_statement(inner, max),
        sql::Statement::Begin | sql::Statement::Commit | sql::Statement::Rollback => {}
    }
}

fn scan_expr(expr: &sql::Expr, max: &mut u32) {
    match expr {
        sql::Expr::Parameter { index } => {
            if *index > *max {
                *max = *index;
            }
        }
        sql::Expr::Literal(_) | sql::Expr::Column { .. } | sql::Expr::SessionContext(_) => {}
        sql::Expr::UnaryOp { expr, .. } => scan_expr(expr, max),
        sql::Expr::IsNull { expr, .. } => scan_expr(expr, max),
        sql::Expr::BinaryOp { left, right, .. } => {
            scan_expr(left, max);
            scan_expr(right, max);
        }
    }
}

fn substitute_statement(statement: sql::Statement, values: &[Value]) -> sql::Statement {
    match statement {
        sql::Statement::Select(select) => sql::Statement::Select(substitute_select(select, values)),
        sql::Statement::Insert(insert) => sql::Statement::Insert(substitute_insert(insert, values)),
        sql::Statement::CreateTable(create) => sql::Statement::CreateTable(create),
        sql::Statement::CreateIndex(create) => sql::Statement::CreateIndex(create),
        sql::Statement::Explain { verbose, inner } => sql::Statement::Explain {
            verbose,
            inner: Box::new(substitute_statement(*inner, values)),
        },
        sql::Statement::Begin => sql::Statement::Begin,
        sql::Statement::Commit => sql::Statement::Commit,
        sql::Statement::Rollback => sql::Statement::Rollback,
    }
}

fn substitute_select(select: sql::SelectStatement, values: &[Value]) -> sql::SelectStatement {
    let items = select
        .items
        .into_iter()
        .map(|item| match item {
            sql::SelectItem::Wildcard => sql::SelectItem::Wildcard,
            sql::SelectItem::Expr(expr) => sql::SelectItem::Expr(substitute_expr(expr, values)),
        })
        .collect();
    let where_clause = select.where_clause.map(|expr| substitute_expr(expr, values));
    sql::SelectStatement { items, from: select.from, where_clause }
}

fn substitute_insert(insert: sql::InsertStatement, values: &[Value]) -> sql::InsertStatement {
    let rows = insert
        .values
        .into_iter()
        .map(|row| row.into_iter().map(|expr| substitute_expr(expr, values)).collect())
        .collect();
    sql::InsertStatement { table: insert.table, columns: insert.columns, values: rows }
}

fn substitute_expr(expr: sql::Expr, values: &[Value]) -> sql::Expr {
    match expr {
        sql::Expr::Parameter { index } => sql::Expr::Literal(values[(index - 1) as usize].clone()),
        sql::Expr::Literal(value) => sql::Expr::Literal(value),
        sql::Expr::SessionContext(name) => sql::Expr::SessionContext(name),
        sql::Expr::Column { table, name } => sql::Expr::Column { table, name },
        sql::Expr::BinaryOp { left, op, right } => sql::Expr::BinaryOp {
            left: Box::new(substitute_expr(*left, values)),
            op,
            right: Box::new(substitute_expr(*right, values)),
        },
        sql::Expr::UnaryOp { op, expr } => {
            sql::Expr::UnaryOp { op, expr: Box::new(substitute_expr(*expr, values)) }
        }
        sql::Expr::IsNull { expr, negated } => {
            sql::Expr::IsNull { expr: Box::new(substitute_expr(*expr, values)), negated }
        }
    }
}
