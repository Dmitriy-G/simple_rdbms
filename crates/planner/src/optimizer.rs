use catalog::Catalog;
use common::{IndexId, TableId};
use sql::BinaryOperator;
use types::{MemcomparableEncode, Value};

use crate::binder::BoundExpr;
use crate::logical_plan::LogicalPlan;

pub trait OptimizerRule {
    fn name(&self) -> &'static str;

    fn apply(&self, plan: LogicalPlan, catalog: &Catalog) -> LogicalPlan;
}

pub struct Optimizer {
    rules: Vec<Box<dyn OptimizerRule>>,
}

impl Optimizer {
    pub fn new(rules: Vec<Box<dyn OptimizerRule>>) -> Self {
        Self { rules }
    }

    pub fn optimize(&self, plan: LogicalPlan, catalog: &Catalog) -> LogicalPlan {
        let plan = self.recurse_into_children(plan, catalog);
        self.rules.iter().fold(plan, |plan, rule| rule.apply(plan, catalog))
    }

    fn recurse_into_children(&self, plan: LogicalPlan, catalog: &Catalog) -> LogicalPlan {
        match plan {
            LogicalPlan::Filter { predicate, input } => {
                LogicalPlan::Filter { predicate, input: Box::new(self.optimize(*input, catalog)) }
            }
            LogicalPlan::Projection { expressions, input } => LogicalPlan::Projection {
                expressions,
                input: Box::new(self.optimize(*input, catalog)),
            },
            LogicalPlan::Join { left, right, predicate } => LogicalPlan::Join {
                left: Box::new(self.optimize(*left, catalog)),
                right: Box::new(self.optimize(*right, catalog)),
                predicate,
            },
            leaf @ (LogicalPlan::SeqScan { .. }
            | LogicalPlan::IndexScan { .. }
            | LogicalPlan::Insert { .. }
            | LogicalPlan::CreateTable { .. }
            | LogicalPlan::CreateIndex { .. }) => leaf,
        }
    }
}

pub struct IndexScanRule;

impl OptimizerRule for IndexScanRule {
    fn name(&self) -> &'static str {
        "index_scan"
    }

    fn apply(&self, plan: LogicalPlan, catalog: &Catalog) -> LogicalPlan {
        match plan {
            LogicalPlan::Filter { predicate, input } => match *input {
                LogicalPlan::SeqScan { table_id } => {
                    match index_scan_bounds(&predicate, table_id, catalog) {
                        Some((index_id, start, end)) => LogicalPlan::Filter {
                            predicate,
                            input: Box::new(LogicalPlan::IndexScan {
                                index_id,
                                table_id,
                                start,
                                end,
                            }),
                        },
                        None => LogicalPlan::Filter {
                            predicate,
                            input: Box::new(LogicalPlan::SeqScan { table_id }),
                        },
                    }
                }
                other => LogicalPlan::Filter { predicate, input: Box::new(other) },
            },
            other => other,
        }
    }
}

fn flatten_and_conjuncts<'a>(expr: &'a BoundExpr, out: &mut Vec<&'a BoundExpr>) {
    if let BoundExpr::BinaryOp { left, op: BinaryOperator::And, right, .. } = expr {
        flatten_and_conjuncts(left, out);
        flatten_and_conjuncts(right, out);
    } else {
        out.push(expr);
    }
}

// TODO(M11): also match `Literal <op> ColumnRef` (the operands reversed) and drop a
// fully-covered equality filter once the index alone can answer it.
fn column_and_literal(expr: &BoundExpr) -> Option<(usize, BinaryOperator, &Value)> {
    let BoundExpr::BinaryOp { left, op, right, .. } = expr else { return None };
    let (BoundExpr::ColumnRef { index, .. }, BoundExpr::Literal(value)) =
        (left.as_ref(), right.as_ref())
    else {
        return None;
    };
    matches!(
        op,
        BinaryOperator::Eq
            | BinaryOperator::Lt
            | BinaryOperator::LtEq
            | BinaryOperator::Gt
            | BinaryOperator::GtEq
    )
    .then_some((*index, *op, value))
}

fn successor(mut bytes: Vec<u8>) -> Vec<u8> {
    bytes.push(0x00);
    bytes
}

type IndexScanBounds = (IndexId, Option<Vec<u8>>, Option<Vec<u8>>);

fn index_scan_bounds(
    predicate: &BoundExpr,
    table_id: TableId,
    catalog: &Catalog,
) -> Option<IndexScanBounds> {
    let mut conjuncts = Vec::new();
    flatten_and_conjuncts(predicate, &mut conjuncts);

    let (target_column, index_id) = conjuncts.iter().find_map(|expr| {
        let (column_index, _, _) = column_and_literal(expr)?;
        let info = catalog.index_for_column(table_id, column_index)?;
        Some((column_index, info.index_id))
    })?;

    let mut start: Option<Vec<u8>> = None;
    let mut end: Option<Vec<u8>> = None;
    for expr in &conjuncts {
        let Some((column_index, op, value)) = column_and_literal(expr) else { continue };
        if column_index != target_column {
            continue;
        }
        let mut encoded = Vec::new();
        if value.encode_memcomparable(&mut encoded).is_err() {
            continue;
        }
        let (candidate_start, candidate_end) = match op {
            BinaryOperator::Eq => (Some(encoded.clone()), Some(successor(encoded))),
            BinaryOperator::Lt => (None, Some(encoded)),
            BinaryOperator::LtEq => (None, Some(successor(encoded))),
            BinaryOperator::Gt => (Some(successor(encoded)), None),
            BinaryOperator::GtEq => (Some(encoded), None),
            _ => unreachable!("column_and_literal only returns comparison operators"),
        };
        if let Some(candidate_start) = candidate_start {
            start = Some(
                start.filter(|current| *current >= candidate_start).unwrap_or(candidate_start),
            );
        }
        if let Some(candidate_end) = candidate_end {
            end = Some(end.filter(|current| *current <= candidate_end).unwrap_or(candidate_end));
        }
    }

    Some((index_id, start, end))
}
