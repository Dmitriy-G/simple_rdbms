# insert

Inserts `rows` into a table's heap file, maintaining every index on that
table as it goes. Yields exactly one output row - a single `BigInt` column
holding the number of rows inserted - then `None`. There is no plan shape
for `INSERT ... SELECT` in this grammar, so unlike the other operators
this is a leaf: `rows` are literal value expressions from the `VALUES`
clause, not pulled from a child executor.

**Index maintenance.** For each row, once its heap `Rid` is known, `next`
iterates `ctx.catalog.indexes_for_table(self.table_id)` and, for each
`IndexInfo`, encodes the row's value at `index.column_index` via
`types::MemcomparableEncode::encode_memcomparable`, fetches that index's
*current* root page via `ctx.catalog.index_root_page` (never cached - see
`catalog::catalog.MD`), and calls `storage::btree::BTreeIndex::insert`.
If that insert changed `BTreeIndex::root_page_id()` (a root split), `next`
immediately calls `ctx.catalog.update_index_root_page` to persist the new
root - the caller-must-notice contract `storage::btree.MD` documents for
`insert`. A `Value::Null` in an indexed column is inserted like any other
value (the `NULL_TAG` byte `types::memcomparable.MD` describes); no
special-casing is needed because NULL-safety for `col = x`/`col > x`
comes entirely from the `Filter` re-checking every row a later
`executor::IndexScanExecutor` returns, not from anything `InsertExecutor`
does. Encoding failure (a NaN `Double`) surfaces as
`storage::StorageError::UnorderableKey`, converted via `?` into
`ExecutorError::Storage` - failing the whole `next` call immediately, mid
loop over `self.rows` if necessary. `next` does not itself roll anything
back on that failure: every write so far (this row's heap insert, any
index inserts before the one that failed) shares the same `TxnId`, so
`engine::Database`'s ordinary abort path (WAL undo, `engine::database.MD`)
unwinds all of it - propagating the error is enough.

## Key Components

- `InsertExecutor` - holds `table_id`, `rows: Vec<Vec<BoundExpr>>` (one
  `BoundExpr` per column of the target table's schema, in schema order,
  for each row of the `VALUES` clause), `first_page_id: Option<PageId>`
  (resolved at `init` time), and `done: bool` (so `next` yields its one
  summary row exactly once).
- `InsertExecutor::new(table_id, rows) -> Self` - creates an insert into
  `table_id` of `rows`.

## Usage Example

```rust
use executor::{Executor, InsertExecutor};

let mut insert = InsertExecutor::new(table_id, rows);
insert.init(&mut ctx)?;
let summary = insert.next(&mut ctx)?; // Some(Tuple([BigInt(row_count)]))
assert!(insert.next(&mut ctx)?.is_none());
```

`engine::executor_factory::build_executor` constructs an `InsertExecutor`
for every `PhysicalPlan::Insert`, always as the root of its plan tree
since it has no child. Insert values reference no columns - the binder
rejects any that do - so `next` evaluates each row's expressions against
an empty tuple.
