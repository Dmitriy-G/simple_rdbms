# error

Errors raised while executing a physical plan.

## Key Components

- `ExecutorError` - `Evaluation(String)` for an expression evaluation
  failure (e.g. a type mismatch the binder should have caught but didn't);
  `Storage(#[from] storage::StorageError)` for an error surfaced by the
  storage layer while an operator read or wrote tuples; `Transaction
  (String)` for a transaction-layer failure such as a lock that could not
  be acquired; `Catalog(String)` for a catalog lookup failure, e.g. a
  table id that no longer resolves.
- `From<ExecutorError> for common::Error` - wraps any `ExecutorError` into
  `common::Error::Execution`, so callers above `executor` (which speak
  `common::Result`) don't need their own conversion.

## Usage Example

```rust
fn init(&mut self, ctx: &mut ExecutorContext<'_>) -> Result<(), ExecutorError> {
    let table = ctx
        .catalog
        .get_table_by_id(self.table_id)
        .map_err(|err| ExecutorError::Catalog(err.to_string()))?;
    Ok(())
}
```

Every `Executor::init`/`next` implementation returns `Result<_,
ExecutorError>`; `?` on a `storage::StorageError` converts automatically
via the `From` impl, and `engine::Database::execute` converts the final
`ExecutorError` into `common::Error` the same way it handles errors from
every other layer.
