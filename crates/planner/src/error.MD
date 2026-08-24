# error

Errors raised while binding a parsed statement against the catalog.

## Key Components

- `PlannerError` - the enum. `UnknownTable(String)` is raised when a
  statement references a table not present in the catalog.
  `UnknownColumn(String)` is raised when a statement references a column
  not present in the resolved table's schema. `ColumnCountMismatch {
  expected, found }` is raised when an `INSERT` row supplies a different
  number of values than the number of target columns (explicit or, if
  omitted, the table's full column list). `TypeMismatch(String)` is raised
  when an expression's operand types are incompatible with the operator
  applied to them. `AmbiguousColumn(String)` is raised when a column
  reference could be resolved against more than one source in scope, and
  no qualifier was given to disambiguate it. `LiteralOutOfRange { column,
  value, data_type }` is raised when a literal being coerced to a column's
  narrower type does not fit in it (e.g. a `BigInt` literal too large for
  an `Integer` column); `column` names the column it was bound against,
  `value` is the literal's original text as written in the source SQL, and
  `data_type` is the column's declared type the literal did not fit in.
- `impl From<PlannerError> for common::Error` - converts into the
  workspace-wide error type as `Error::Binder`, carrying the original
  error's `Display` text.

## Usage Example

```rust
use planner::PlannerError;

match Binder::new(&catalog).bind(statement) {
    Err(PlannerError::UnknownTable(name)) => eprintln!("no such table: {name}"),
    _ => {}
}
```

Every fallible `Binder` method returns `Result<_, PlannerError>`; callers
in `engine` propagate it with `?` and rely on the `From` impl to fold it
into `common::Error` at the point it crosses into `Database::execute`.
