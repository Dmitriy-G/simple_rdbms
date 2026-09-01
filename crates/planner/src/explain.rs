use catalog::Catalog;
use common::{IndexId, TableId};
use sql::{BinaryOperator, UnaryOperator};
use types::{DataType, Value, decode_memcomparable};

use crate::binder::BoundExpr;
use crate::logical_plan::{self, LogicalPlan};
use crate::physical_plan::{self, PhysicalPlan};

pub(crate) type ColumnList = Vec<(String, Option<DataType>)>;

pub fn explain_logical(plan: &LogicalPlan, catalog: &Catalog, verbose: bool) -> Vec<String> {
    let mut out = Vec::new();
    logical_plan::render(plan, catalog, verbose, 0, &mut out);
    out
}

pub fn explain_physical(plan: &PhysicalPlan, catalog: &Catalog, verbose: bool) -> Vec<String> {
    let mut out = Vec::new();
    physical_plan::render(plan, catalog, verbose, 0, &mut out);
    out
}

pub(crate) fn table_name(table_id: TableId, catalog: &Catalog) -> String {
    catalog
        .get_table_by_id(table_id)
        .map(|info| info.name.clone())
        .unwrap_or_else(|_| format!("<table {}>", table_id.0))
}

pub(crate) fn index_name(index_id: IndexId, table_id: TableId, catalog: &Catalog) -> String {
    catalog
        .indexes_for_table(table_id)
        .find(|info| info.index_id == index_id)
        .map(|info| info.name.clone())
        .unwrap_or_else(|| format!("<index {}>", index_id.0))
}

pub(crate) fn column_name(table_id: TableId, column_index: usize, catalog: &Catalog) -> String {
    catalog
        .get_table_by_id(table_id)
        .ok()
        .and_then(|info| info.schema.columns().get(column_index))
        .map(|column| column.name.clone())
        .unwrap_or_else(|| format!("<column {column_index}>"))
}

pub(crate) fn table_columns(table_id: TableId, catalog: &Catalog) -> ColumnList {
    catalog
        .get_table_by_id(table_id)
        .map(|info| {
            info.schema
                .columns()
                .iter()
                .map(|column| (column.name.clone(), Some(column.data_type)))
                .collect()
        })
        .unwrap_or_default()
}

pub(crate) fn projected_column(
    expr: &BoundExpr,
    input_cols: &[(String, Option<DataType>)],
    position: usize,
) -> (String, Option<DataType>) {
    match expr {
        BoundExpr::ColumnRef { index, data_type } => {
            let name = input_cols
                .get(*index)
                .map(|(name, _)| name.clone())
                .unwrap_or_else(|| format!("column{}", position + 1));
            (name, Some(*data_type))
        }
        BoundExpr::Literal(value) => (format!("column{}", position + 1), value.data_type()),
        BoundExpr::BinaryOp { data_type, .. } | BoundExpr::UnaryOp { data_type, .. } => {
            (format!("column{}", position + 1), Some(*data_type))
        }
        BoundExpr::IsNull { .. } => (format!("column{}", position + 1), Some(DataType::Boolean)),
    }
}

pub(crate) fn cols_line(cols: &[(String, Option<DataType>)]) -> String {
    let names: Vec<&str> = cols.iter().map(|(name, _)| name.as_str()).collect();
    format!("cols=[{}]", names.join(", "))
}

pub(crate) fn verbose_cols_line(cols: &[(String, Option<DataType>)]) -> String {
    let rendered: Vec<String> = cols
        .iter()
        .map(|(name, data_type)| match data_type {
            Some(data_type) => format!("{name}:{}", format_data_type(*data_type)),
            None => format!("{name}:NULL"),
        })
        .collect();
    format!("cols=[{}]", rendered.join(", "))
}

pub(crate) fn push_verbose_line(out: &mut Vec<String>, depth: usize, parts: Vec<String>) {
    if parts.is_empty() {
        return;
    }
    let indent = "  ".repeat(depth + 1);
    out.push(format!("{indent}{}", parts.join(" ")));
}

pub(crate) fn format_data_type(data_type: DataType) -> String {
    match data_type {
        DataType::Boolean => "BOOLEAN".to_string(),
        DataType::Integer => "INTEGER".to_string(),
        DataType::BigInt => "BIGINT".to_string(),
        DataType::Double => "DOUBLE".to_string(),
        DataType::Varchar(u32::MAX) => "TEXT".to_string(),
        DataType::Varchar(len) => format!("VARCHAR({len})"),
    }
}

pub(crate) fn render_expr(expr: &BoundExpr, cols: &[(String, Option<DataType>)]) -> String {
    match expr {
        BoundExpr::Literal(value) => render_value(value),
        BoundExpr::ColumnRef { index, .. } => {
            cols.get(*index).map(|(name, _)| name.clone()).unwrap_or_else(|| format!("col{index}"))
        }
        BoundExpr::UnaryOp { op, expr, .. } => render_unary(*op, expr, cols),
        BoundExpr::BinaryOp { left, op, right, .. } => {
            format!("{} {} {}", wrapped(left, cols), op_symbol(*op), wrapped(right, cols))
        }
        BoundExpr::IsNull { expr, negated } => {
            format!("{} IS {}NULL", wrapped(expr, cols), if *negated { "NOT " } else { "" })
        }
    }
}

fn wrapped(expr: &BoundExpr, cols: &[(String, Option<DataType>)]) -> String {
    match expr {
        BoundExpr::BinaryOp { .. } | BoundExpr::UnaryOp { .. } | BoundExpr::IsNull { .. } => {
            format!("({})", render_expr(expr, cols))
        }
        BoundExpr::Literal(_) | BoundExpr::ColumnRef { .. } => render_expr(expr, cols),
    }
}

fn render_unary(
    op: UnaryOperator,
    expr: &BoundExpr,
    cols: &[(String, Option<DataType>)],
) -> String {
    match op {
        UnaryOperator::Not => format!("NOT {}", wrapped(expr, cols)),
        UnaryOperator::Negate => format!("-{}", wrapped(expr, cols)),
    }
}

fn op_symbol(op: BinaryOperator) -> &'static str {
    match op {
        BinaryOperator::Eq => "=",
        BinaryOperator::NotEq => "<>",
        BinaryOperator::Lt => "<",
        BinaryOperator::LtEq => "<=",
        BinaryOperator::Gt => ">",
        BinaryOperator::GtEq => ">=",
        BinaryOperator::And => "AND",
        BinaryOperator::Or => "OR",
        BinaryOperator::Plus => "+",
        BinaryOperator::Minus => "-",
        BinaryOperator::Multiply => "*",
        BinaryOperator::Divide => "/",
    }
}

fn render_value(value: &Value) -> String {
    match value {
        Value::Null => "NULL".to_string(),
        Value::Boolean(b) => if *b { "TRUE" } else { "FALSE" }.to_string(),
        Value::Integer(v) => v.to_string(),
        Value::BigInt(v) => v.to_string(),
        Value::Double(v) => v.to_string(),
        Value::Varchar(s) => format!("'{}'", s.replace('\'', "''")),
    }
}

fn is_successor(start: &[u8], end: &[u8]) -> bool {
    end.len() == start.len() + 1 && end[..start.len()] == *start && end[start.len()] == 0x00
}

fn decode_bound(bytes: &[u8], data_type: DataType) -> Option<(Value, bool)> {
    let (value, consumed) = decode_memcomparable(bytes, data_type).ok()?;
    match bytes.len() - consumed {
        0 => Some((value, false)),
        1 if bytes[consumed] == 0x00 => Some((value, true)),
        _ => None,
    }
}

pub(crate) fn render_index_cond(
    index_id: IndexId,
    table_id: TableId,
    start: &Option<Vec<u8>>,
    end: &Option<Vec<u8>>,
    catalog: &Catalog,
) -> String {
    render_index_cond_inner(index_id, table_id, start, end, catalog)
        .unwrap_or_else(|| "<undecodable>".to_string())
}

fn render_index_cond_inner(
    index_id: IndexId,
    table_id: TableId,
    start: &Option<Vec<u8>>,
    end: &Option<Vec<u8>>,
    catalog: &Catalog,
) -> Option<String> {
    let info = catalog.indexes_for_table(table_id).find(|info| info.index_id == index_id)?;
    let table = catalog.get_table_by_id(table_id).ok()?;
    let column = table.schema.columns().get(info.column_index)?;
    let column_name = &column.name;
    let data_type = column.data_type;

    match (start, end) {
        (None, None) => Some("(no bounds)".to_string()),
        (Some(s), Some(e)) if is_successor(s, e) => {
            let (value, _) = decode_memcomparable(s, data_type).ok()?;
            Some(format!("{column_name} = {}", render_value(&value)))
        }
        _ => {
            let mut parts = Vec::new();
            if let Some(s) = start {
                let (value, has_sentinel) = decode_bound(s, data_type)?;
                let op = if has_sentinel { ">" } else { ">=" };
                parts.push(format!("{column_name} {op} {}", render_value(&value)));
            }
            if let Some(e) = end {
                let (value, has_sentinel) = decode_bound(e, data_type)?;
                let op = if has_sentinel { "<=" } else { "<" };
                parts.push(format!("{column_name} {op} {}", render_value(&value)));
            }
            Some(parts.join(" AND "))
        }
    }
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" ")
}
