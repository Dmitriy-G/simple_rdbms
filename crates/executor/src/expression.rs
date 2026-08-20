use planner::BoundExpr;
use types::{Tuple, Value};

use crate::error::ExecutorError;

/// Evaluates a bound expression against `tuple`, producing the resulting
/// runtime value. Column references are resolved by ordinal position
/// against `tuple`'s schema-ordered values.
pub fn evaluate(expr: &BoundExpr, tuple: &Tuple) -> Result<Value, ExecutorError> {
    let _ = (expr, tuple);
    todo!("match on BoundExpr: literals return directly, ColumnRef indexes into tuple")
}
