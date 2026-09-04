# seq_scan

Reads every tuple of a table's heap file, in physical storage order. The
leaf operator for any plan that reads a table without an index.

Holds only a cursor (`current_page`, `next_slot`) rather than a
materialized copy of the table: each `next` fetches the current page,
reads the next live slot on it, decodes one tuple, and drops the page's
guard before returning, following `next_page_id` once a page is
exhausted. Memory use is therefore independent of table size, and no page
stays pinned across a `next` call.

`init` takes `ctx.lock_manager.lock_table(ctx.txn.txn_id, table_id,
LockMode::Shared)` before resolving the table's schema, and each `next`
that returns a tuple takes `ctx.lock_manager.lock(ctx.txn.txn_id,
Rid::new(page_id, slot), LockMode::Shared)` on that row before decoding
it - see `txn::lock_manager.MD`. Under strict two-phase locking neither
lock releases until the transaction commits or aborts
(`txn::manager.MD`), so a full scan holds a growing set of row locks for
its whole duration; that's the cost of read consistency until MVCC
(`docs/ROADMAP.md`'s M10.3) replaces it.

## Key Components

- `SeqScanExecutor` - holds `table_id`, the target table's `column_types`
  (resolved at `init` time, needed to decode raw tuple bytes),
  `current_page: Option<PageId>` (the cursor; `None` once the scan is
  exhausted), and `next_slot: u16` (the next slot to try on
  `current_page`).
- `SeqScanExecutor::new(table_id) -> Self` - creates a scan over
  `table_id`, with the cursor left unset until `init` runs.

## Usage Example

```rust
use executor::{Executor, SeqScanExecutor};

let mut scan = SeqScanExecutor::new(table_id);
scan.init(&mut ctx)?;
while let Some(tuple) = scan.next(&mut ctx)? {
    // one row of the table, in storage order
}
```

`engine::executor_factory::build_executor` constructs a `SeqScanExecutor`
for every `PhysicalPlan::SeqScan`, and it is nearly always the innermost
operator in a plan tree: `FilterExecutor` and `ProjectionExecutor` pull
from it directly, and `NestedLoopJoinExecutor` will use one as each side
of its join once implemented.
