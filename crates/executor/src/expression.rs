use std::cmp::Ordering;

use planner::{BinaryOperator, BoundExpr, UnaryOperator};
use types::{Tuple, Value};

use crate::error::ExecutorError;

pub fn evaluate(expr: &BoundExpr, tuple: &Tuple) -> Result<Value, ExecutorError> {
    match expr {
        BoundExpr::Literal(value) => Ok(value.clone()),
        BoundExpr::ColumnRef { index, .. } => {
            let values = tuple.values();
            values.get(*index).cloned().ok_or_else(|| {
                ExecutorError::Evaluation(format!(
                    "column index {index} out of bounds for a {}-column tuple",
                    values.len()
                ))
            })
        }
        BoundExpr::UnaryOp { op, expr, .. } => evaluate_unary(*op, expr, tuple),
        BoundExpr::BinaryOp { left, op, right, .. } => evaluate_binary(left, *op, right, tuple),
    }
}

fn evaluate_unary(
    op: UnaryOperator,
    expr: &BoundExpr,
    tuple: &Tuple,
) -> Result<Value, ExecutorError> {
    let operand = evaluate(expr, tuple)?;
    match op {
        UnaryOperator::Not => Ok(match operand {
            Value::Null => Value::Null,
            Value::Boolean(b) => Value::Boolean(!b),
            other => {
                return Err(ExecutorError::Evaluation(format!(
                    "NOT requires a Boolean operand, found {other:?}"
                )));
            }
        }),
        UnaryOperator::Negate => Ok(match operand {
            Value::Null => Value::Null,
            Value::Integer(v) => Value::Integer(-v),
            Value::BigInt(v) => Value::BigInt(-v),
            Value::Double(v) => Value::Double(-v),
            other => {
                return Err(ExecutorError::Evaluation(format!(
                    "unary - requires a numeric operand, found {other:?}"
                )));
            }
        }),
    }
}

fn evaluate_binary(
    left: &BoundExpr,
    op: BinaryOperator,
    right: &BoundExpr,
    tuple: &Tuple,
) -> Result<Value, ExecutorError> {
    match op {
        BinaryOperator::And => {
            let left = evaluate(left, tuple)?;
            let right = evaluate(right, tuple)?;
            three_valued_and(left, right)
        }
        BinaryOperator::Or => {
            let left = evaluate(left, tuple)?;
            let right = evaluate(right, tuple)?;
            three_valued_or(left, right)
        }
        BinaryOperator::Eq
        | BinaryOperator::NotEq
        | BinaryOperator::Lt
        | BinaryOperator::LtEq
        | BinaryOperator::Gt
        | BinaryOperator::GtEq => {
            let left = evaluate(left, tuple)?;
            let right = evaluate(right, tuple)?;
            let ordering =
                left.compare(&right).map_err(|err| ExecutorError::Evaluation(err.to_string()))?;
            Ok(match ordering {
                None => Value::Null,
                Some(ordering) => Value::Boolean(matches_comparison(op, ordering)),
            })
        }
        BinaryOperator::Plus
        | BinaryOperator::Minus
        | BinaryOperator::Multiply
        | BinaryOperator::Divide => {
            Err(ExecutorError::NotSupported("arithmetic operators are not supported".to_string()))
        }
    }
}

fn matches_comparison(op: BinaryOperator, ordering: Ordering) -> bool {
    match op {
        BinaryOperator::Eq => ordering == Ordering::Equal,
        BinaryOperator::NotEq => ordering != Ordering::Equal,
        BinaryOperator::Lt => ordering == Ordering::Less,
        BinaryOperator::LtEq => ordering != Ordering::Greater,
        BinaryOperator::Gt => ordering == Ordering::Greater,
        BinaryOperator::GtEq => ordering != Ordering::Less,
        _ => unreachable!("matches_comparison only called for comparison operators"),
    }
}

fn three_valued_and(left: Value, right: Value) -> Result<Value, ExecutorError> {
    match (as_bool(left)?, as_bool(right)?) {
        (Some(false), _) | (_, Some(false)) => Ok(Value::Boolean(false)),
        (Some(true), Some(true)) => Ok(Value::Boolean(true)),
        _ => Ok(Value::Null),
    }
}

fn three_valued_or(left: Value, right: Value) -> Result<Value, ExecutorError> {
    match (as_bool(left)?, as_bool(right)?) {
        (Some(true), _) | (_, Some(true)) => Ok(Value::Boolean(true)),
        (Some(false), Some(false)) => Ok(Value::Boolean(false)),
        _ => Ok(Value::Null),
    }
}

fn as_bool(value: Value) -> Result<Option<bool>, ExecutorError> {
    match value {
        Value::Null => Ok(None),
        Value::Boolean(b) => Ok(Some(b)),
        other => Err(ExecutorError::Evaluation(format!(
            "AND/OR require Boolean operands, found {other:?}"
        ))),
    }
}
