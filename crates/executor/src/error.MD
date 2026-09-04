# error

Errors raised while executing a physical plan.

## Key Components

- `ExecutorError` - `Evaluation(String)` for an expression evaluation
  failure (e.g. a type mismatch the binder should have caught but didn't,
  or a column index out of bounds for the current tuple).
  `CorruptTuple(String)` for a decoded row that doesn't match its table's
  schema - a stored tuple's bytes are wrong in a way the binder can't have
  caused, so it's classified as data corruption rather than an ordinary
  evaluation failure. `NotSupported(String)` for a well-formed request this
  engine doesn't implement yet (e.g. arithmetic operators in an
  expression). `Storage(#[from] storage::StorageError)` for an error
  surfaced by the storage layer while an operator read or wrote tuples.
  `Catalog(#[from] catalog::CatalogError)` for a catalog lookup failure,
  e.g. a table id that no longer resolves. `Lock(#[from] txn::TxnError)`
  for a failure acquiring a table or row lock (`context.MD`'s
  `lock_manager`) - a deadlock victim or a lock request made after
  `TransactionManager::release_all` already ran for this transaction.
- `From<ExecutorError> for common::Error` - `Evaluation` becomes
  `DatatypeMismatch`, `CorruptTuple` becomes `DataCorrupted`,
  `NotSupported` becomes the identically-named `common::Error` variant,
  and `Storage`/`Catalog`/`Lock` each delegate to their own crate's
  conversion - so callers above `executor` (which speak `common::Result`)
  don't need their own conversion.

## Usage Example

```rust
fn init(&mut self, ctx: &mut ExecutorContext<'_>) -> Result<(), ExecutorError> {
    let table = ctx.catalog.get_table_by_id(self.table_id)?;
    Ok(())
}
```

Every `Executor::init`/`next` implementation returns `Result<_,
ExecutorError>`; `?` on a `storage::StorageError` or `catalog::CatalogError`
converts automatically via their `#[from]` variants, and
`engine::Database::execute` converts the final `ExecutorError` into
`common::Error` the same way it handles errors from every other layer.
