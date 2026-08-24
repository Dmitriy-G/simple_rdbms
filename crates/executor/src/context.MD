# context

The shared state every operator in a plan tree needs while executing: the
catalog for schema lookups, the buffer pool for reading and writing pages,
and the transaction on whose behalf the plan is running (for visibility
checks and lock acquisition). Passed down through `next` calls rather than
owned by individual operators, so the same context serves the whole tree.

## Key Components

- `ExecutorContext<'a>` - holds `catalog: &'a Catalog`, `buffer_pool: &'a
  BufferPool`, and `txn: &'a Transaction`. `buffer_pool` is a shared
  (not exclusive) reference because `BufferPool` uses interior mutability
  throughout: this is what lets an operator like `SeqScanExecutor` hold
  cursor state across `next` calls without needing an exclusive borrow
  re-acquired on every call.
- `ExecutorContext::new(catalog, buffer_pool, txn) -> Self` - builds a
  context from its parts.

## Usage Example

```rust
use executor::ExecutorContext;

let mut ctx = ExecutorContext::new(&catalog, &buffer_pool, txn);
executor.init(&mut ctx)?;
```

`engine::Database::run` builds exactly one `ExecutorContext` per statement
and passes it by mutable reference into every `init`/`next` call on the
operator tree, so all operators in a plan see the same catalog snapshot,
buffer pool, and transaction.
