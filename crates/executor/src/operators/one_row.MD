# one_row

Produces exactly one row of zero columns and then ends. It is the source
a `SELECT` with no `FROM` clause sits on: `SELECT 1` and
`SELECT 'keep alive'` are a projection of literal expressions over a
single anonymous row, which is what makes a JDBC-family client's
keep-alive query ordinary SQL here rather than a special case answered by
the wire layer. It reads nothing, takes no table lock and touches neither
the catalog nor the buffer pool, so a from-less `SELECT` costs no I/O and
blocks on nothing - see `projection.MD` for the operator that sits above
it and `seq_scan.MD` for the source it replaces when a table is named.

## Key Components

- `OneRowExecutor` - holds a single `emitted` flag, which is the whole of
  its state: false until the one row has been handed out, true after.
- `OneRowExecutor::new() -> Self` - creates the operator, unstarted.
  `init` resets `emitted`, so a re-initialized operator yields its row
  again rather than being permanently exhausted.

## Usage Example

```rust
use executor::{Executor, OneRowExecutor, ProjectionExecutor};

let source = OneRowExecutor::new();
let projected = ProjectionExecutor::new(select_list, Box::new(source));
```

`engine::executor_factory::build_executor` constructs a `OneRowExecutor`
for every `PhysicalPlan::OneRow`, which `planner::plan` emits when a
`BoundSelect` carries no `table_id`. The projection above it evaluates
its expressions against the empty tuple this yields, so a from-less
`SELECT` returns exactly one row - and a `WHERE` clause, which the binder
allows with no `FROM` exactly as PostgreSQL does, becomes a `Filter`
between the two that can reduce that row to none.
