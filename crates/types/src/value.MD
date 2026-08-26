# value

A runtime value: one cell of a tuple. Every SQL literal, column value, and
expression result is a `Value`. `Value::Null` is the single representation
of SQL `NULL` and is valid regardless of the column's declared `DataType`.

## Key Components

- `Value` - the enum. `Null` is SQL `NULL`: the absence of a value,
  distinct from any typed value. `Boolean(bool)`, `Integer(i32)`,
  `BigInt(i64)`, `Double(f64)`, and `Varchar(String)` each carry the
  value's own data directly.
- `Value::data_type(&self) -> Option<DataType>` - the `DataType` this
  value would occupy, or `None` for `Value::Null`, which has no type of
  its own until unified with a column or another operand's type.
- `Value::is_null(&self) -> bool` - whether this value is `Value::Null`.
- `Value::compare(&self, other: &Value) -> Result<Option<Ordering>, ValueError>` -
  three-valued-logic comparison against another value, following SQL
  comparison semantics: comparisons involving `NULL` yield `None` rather
  than a definite ordering, and comparing across incompatible types is an
  error.
- `Value::type_name(&self) -> String` - private. A short name for this
  value's variant, used in error messages.
- `ValueError` - errors raised while operating on `Value`s. `TypeMismatch
  { lhs, rhs }` names the two operands' type names when they cannot be
  compared or coerced to a common type. `UnorderableValue { reason }` is
  raised by `MemcomparableEncode::encode_memcomparable` (`memcomparable.MD`)
  for a `Value` with no defined ordering - today, only a NaN `Double`,
  since SQL itself does not order NaN. `InvalidEncoding { reason }` is
  raised by `decode_memcomparable` (`memcomparable.MD`) when the bytes
  handed to it are not a well-formed memcomparable encoding: too short for
  the tag or value it claims to hold, an unrecognized tag byte, or (for
  `Varchar`) bytes that are not valid UTF-8.

## Usage Example

```rust
use types::Value;

let ordering = Value::Integer(1).compare(&Value::Integer(2))?;
assert_eq!(ordering, Some(std::cmp::Ordering::Less));
```

`executor::expression::evaluate` calls `Value::compare` to implement every
`=`/`<`/`<=`/`>`/`>=`/`<>` comparison in a `WHERE` clause, turning `None`
(a `NULL` operand) into `Value::Null` per SQL's three-valued logic rather
than a boolean; `planner::Binder` calls `Value::data_type` while coercing
literals against a column's declared type during binding.
