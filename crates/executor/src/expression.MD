# expression

Evaluates a `planner::BoundExpr` against a `types::Tuple`, producing the
resulting runtime `Value`. Column references are resolved by ordinal
position against the tuple's schema-ordered values.

Follows SQL three-valued logic throughout: a comparison with a `NULL`
operand yields `Value::Null` rather than `true`/`false`, and `AND`/`OR`
only short-circuit to a definite result when one operand is definite
enough to decide the outcome regardless of the other (`NULL AND false` is
`false`, `NULL OR true` is `true`); otherwise they too yield `NULL`.
Arithmetic operators (`+ - * /`) are recognized by the grammar and binder
but not yet supported here — evaluating one is an `ExecutorError`.

## Key Components

- `evaluate(expr, tuple) -> Result<Value, ExecutorError>` - evaluates any
  `BoundExpr` variant (`Literal`, `ColumnRef`, `UnaryOp`, `BinaryOp`)
  against `tuple`.
- `evaluate_unary(op, expr, tuple)` - private. Evaluates `NOT` (`NULL` in,
  `NULL` out; flips a `Boolean`) and unary `-` (negates `Integer`,
  `BigInt`, or `Double`; propagates `NULL`).
- `evaluate_binary(left, op, right, tuple)` - private. Dispatches `AND`/
  `OR` to the three-valued helpers, the comparison operators to
  `Value::compare` plus `matches_comparison`, and rejects arithmetic
  operators as unsupported.
- `matches_comparison(op, ordering) -> bool` - private. Maps a `Some`
  `Ordering` from `Value::compare` to the boolean result of the specific
  comparison operator (`Eq`, `Lt`, `GtEq`, ...).
- `three_valued_and(left, right) -> Result<Value, ExecutorError>` -
  private. SQL three-valued `AND`: `false` on either side is decisive
  regardless of the other operand; only when neither side is definitely
  `false` does a `NULL` operand make the result `NULL` instead of `true`.
- `three_valued_or(left, right) -> Result<Value, ExecutorError>` -
  private. SQL three-valued `OR`: `true` on either side is decisive
  regardless of the other operand; only when neither side is definitely
  `true` does a `NULL` operand make the result `NULL` instead of `false`.
- `as_bool(value) -> Result<Option<bool>, ExecutorError>` - private.
  Unwraps a `Value` expected to be `Boolean` or `Null` into `Option<bool>`
  for the three-valued helpers, erroring on any other variant.

## Usage Example

```rust
use executor::evaluate;

let value = evaluate(&predicate, &tuple)?;
if value == types::Value::Boolean(true) {
    // row passes the predicate
}
```

`FilterExecutor::next` calls `evaluate` on its predicate for every row
pulled from its child, keeping only those that evaluate to
`Value::Boolean(true)` (a `NULL` result, like `false`, excludes the row);
`ProjectionExecutor::next` calls it once per output expression to build
the projected tuple; `InsertExecutor::next` calls it against an empty
tuple to turn each `VALUES` row's literal expressions into stored values.
