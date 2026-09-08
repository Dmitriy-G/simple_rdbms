# context

The shared state every operator in a plan tree needs while executing: the
catalog for schema lookups, the buffer pool for reading and writing pages,
the transaction on whose behalf the plan is running (for visibility
checks and lock acquisition), the lock manager operators acquire table
and row locks through, and the MVCC version store operators consult for
snapshot-isolation visibility instead of locking. Passed down through
`next` calls rather than owned by individual operators, so the same
context serves the whole tree.

## Key Components

- `ExecutorContext<'a>` - holds `catalog: &'a Catalog`, `buffer_pool: &'a
  BufferPool`, `txn: &'a Transaction`, `lock_manager: &'a
  txn::LockManager`, and `version_store: &'a txn::VersionStore`.
  `buffer_pool` is a shared (not exclusive) reference because `BufferPool`
  uses interior mutability throughout: this is what lets an operator like
  `SeqScanExecutor` hold cursor state across `next` calls without needing
  an exclusive borrow re-acquired on every call. `lock_manager` is the
  same instance every transaction against this database shares
  (`txn::TransactionManager::lock_manager`), so a lock an operator takes
  on `txn`'s behalf is visible to every other transaction checking the
  same resource. `version_store` is likewise the same instance every
  transaction shares (`txn::TransactionManager::version_store`): an
  insert records its new row's version there, and a scan under
  `txn::IsolationLevel::SnapshotIsolation` asks it whether a row it read
  from the heap is visible to `txn`'s snapshot rather than taking a
  shared row lock.
- `ExecutorContext::new(catalog, buffer_pool, txn, lock_manager, version_store) -> Self` -
  builds a context from its parts.

## Usage Example

```rust
use executor::ExecutorContext;

let mut ctx = ExecutorContext::new(&catalog, &buffer_pool, txn, &lock_manager, &version_store);
executor.init(&mut ctx)?;
```

`engine::EngineShared::run` builds exactly one `ExecutorContext` per
statement and passes it by mutable reference into every `init`/`next` call
on the operator tree, so all operators in a plan see the same catalog,
buffer pool, and transaction.

`catalog` stays a `&Catalog` rather than becoming an owned handle, but it
is no longer a *snapshot*: since ADR 0014
(`docs/adr/0014-catalog-is-a-live-shared-object.md`) there is one live
`Catalog` that is never replaced, it synchronizes itself, and the context
holds no lock on it. Each lookup an operator makes returns an owned
`TableInfo`/`IndexInfo` read at the moment of that call, so two calls
within one statement may observe different catalog states if DDL commits
in between - acceptable while DDL runs under an exclusive table lock, and
recorded as the limit to revisit if DDL ever becomes concurrent with DML
on the same table.
