# insert

Inserts `rows` into a table's heap file. Yields exactly one output row - a
single `BigInt` column holding the number of rows inserted - then `None`.
There is no plan shape for `INSERT ... SELECT` in this grammar, so unlike
the other operators this is a leaf: `rows` are literal value expressions
from the `VALUES` clause, not pulled from a child executor.

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
