use catalog::Catalog;
use common::{IndexId, TableId};

use crate::binder::{BoundColumnDef, BoundExpr};
use crate::explain::{self, ColumnList};

#[derive(Debug, Clone)]
pub enum LogicalPlan {
    SeqScan { table_id: TableId },
    IndexScan { index_id: IndexId, table_id: TableId, start: Option<Vec<u8>>, end: Option<Vec<u8>> },
    Filter { predicate: BoundExpr, input: Box<LogicalPlan> },
    Projection { expressions: Vec<BoundExpr>, input: Box<LogicalPlan> },
    Join { left: Box<LogicalPlan>, right: Box<LogicalPlan>, predicate: BoundExpr },
    Insert { table_id: TableId, rows: Vec<Vec<BoundExpr>> },
    CreateTable { table_name: String, columns: Vec<BoundColumnDef> },
    CreateIndex { index_name: String, table_id: TableId, column_index: usize },
}

pub(crate) fn output_columns(plan: &LogicalPlan, catalog: &Catalog) -> ColumnList {
    match plan {
        LogicalPlan::SeqScan { table_id } | LogicalPlan::IndexScan { table_id, .. } => {
            explain::table_columns(*table_id, catalog)
        }
        LogicalPlan::Filter { input, .. } => output_columns(input, catalog),
        LogicalPlan::Projection { expressions, input } => {
            let input_cols = output_columns(input, catalog);
            expressions
                .iter()
                .enumerate()
                .map(|(position, expr)| explain::projected_column(expr, &input_cols, position))
                .collect()
        }
        LogicalPlan::Join { left, right, .. } => {
            let mut cols = output_columns(left, catalog);
            cols.extend(output_columns(right, catalog));
            cols
        }
        LogicalPlan::Insert { .. }
        | LogicalPlan::CreateTable { .. }
        | LogicalPlan::CreateIndex { .. } => Vec::new(),
    }
}

pub(crate) fn render(
    plan: &LogicalPlan,
    catalog: &Catalog,
    verbose: bool,
    depth: usize,
    out: &mut Vec<String>,
) {
    let indent = "  ".repeat(depth);
    match plan {
        LogicalPlan::SeqScan { table_id } => {
            out.push(format!(
                "{indent}Seq Scan  table={}",
                explain::table_name(*table_id, catalog)
            ));
            if verbose {
                let cols = explain::table_columns(*table_id, catalog);
                explain::push_verbose_line(
                    out,
                    depth,
                    vec![explain::verbose_cols_line(&cols), format!("table_id={}", table_id.0)],
                );
            }
        }
        LogicalPlan::IndexScan { index_id, table_id, start, end } => {
            out.push(format!(
                "{indent}Index Scan  table={} index={}",
                explain::table_name(*table_id, catalog),
                explain::index_name(*index_id, *table_id, catalog)
            ));
            out.push(format!(
                "{indent}  Index Cond: {}",
                explain::render_index_cond(*index_id, *table_id, start, end, catalog)
            ));
            if verbose {
                let cols = explain::table_columns(*table_id, catalog);
                let mut parts = vec![
                    explain::verbose_cols_line(&cols),
                    format!("table_id={} index_id={}", table_id.0, index_id.0),
                ];
                if let Some(start) = start {
                    parts.push(format!("start=0x{}", explain::hex(start)));
                }
                if let Some(end) = end {
                    parts.push(format!("end=0x{}", explain::hex(end)));
                }
                explain::push_verbose_line(out, depth, parts);
            }
        }
        LogicalPlan::Filter { predicate, input } => {
            let cols = output_columns(input, catalog);
            out.push(format!("{indent}Filter  {}", explain::render_expr(predicate, &cols)));
            if verbose {
                explain::push_verbose_line(out, depth, vec![explain::verbose_cols_line(&cols)]);
            }
            render(input, catalog, verbose, depth + 1, out);
        }
        LogicalPlan::Projection { expressions, input } => {
            let input_cols = output_columns(input, catalog);
            let output_cols: ColumnList = expressions
                .iter()
                .enumerate()
                .map(|(position, expr)| explain::projected_column(expr, &input_cols, position))
                .collect();
            out.push(format!("{indent}Projection  {}", explain::cols_line(&output_cols)));
            if verbose {
                explain::push_verbose_line(
                    out,
                    depth,
                    vec![explain::verbose_cols_line(&output_cols)],
                );
            }
            render(input, catalog, verbose, depth + 1, out);
        }
        LogicalPlan::Join { left, right, predicate } => {
            let mut cols = output_columns(left, catalog);
            cols.extend(output_columns(right, catalog));
            out.push(format!("{indent}Nested Loop  {}", explain::render_expr(predicate, &cols)));
            if verbose {
                explain::push_verbose_line(out, depth, vec![explain::verbose_cols_line(&cols)]);
            }
            render(left, catalog, verbose, depth + 1, out);
            render(right, catalog, verbose, depth + 1, out);
        }
        LogicalPlan::Insert { table_id, rows } => {
            out.push(format!(
                "{indent}Insert  table={} rows={}",
                explain::table_name(*table_id, catalog),
                rows.len()
            ));
            if verbose {
                explain::push_verbose_line(out, depth, vec![format!("table_id={}", table_id.0)]);
            }
        }
        LogicalPlan::CreateTable { table_name, columns } => {
            let cols = columns
                .iter()
                .map(|column| {
                    format!("{} {}", column.name, explain::format_data_type(column.data_type))
                })
                .collect::<Vec<_>>()
                .join(", ");
            out.push(format!("{indent}Create Table  table={table_name} cols=[{cols}]"));
        }
        LogicalPlan::CreateIndex { index_name, table_id, column_index } => {
            out.push(format!(
                "{indent}Create Index  index={index_name} table={} column={}",
                explain::table_name(*table_id, catalog),
                explain::column_name(*table_id, *column_index, catalog)
            ));
            if verbose {
                explain::push_verbose_line(out, depth, vec![format!("table_id={}", table_id.0)]);
            }
        }
    }
}
