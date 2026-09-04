# index_scan

Reads the rows of a table whose indexed column falls within `[start, end)`
via a `storage::btree::BTreeIndex`, fetching each matching row from the
table's heap by `Rid`. The leaf operator `planner::optimizer::IndexScanRule`
introduces in place of a `seq_scan.MD`'s `SeqScanExecutor` when a
`WHERE` predicate can use an index.

Like `SeqScanExecutor`, holds only a cursor rather than a materialized
result set - but the cursor here is `(current_leaf: Option<PageId>,
next_after: Option<Vec<u8>>)` over B+tree leaf pages instead of heap
pages. It cannot hold a `BTreeIndex` (or
`storage::btree::BTreeRangeIterator`) as a field at all: the `Executor`
trait reborrows `ExecutorContext` fresh on every `init`/`next` call
(`ctx: &mut ExecutorContext<'_>`, a new elided lifetime each call), so
nothing borrowed from `ctx.buffer_pool` can survive from one call to the
next - only owned data (`PageId`, `Vec<u8>`) can live in executor struct
fields. `init` builds a short-lived, local `BTreeIndex` just long enough
to call `leaf_for_start`, then drops it, and seeds `next_after` from its
own `start` bound (`None` for an unbounded scan); `next` drives the
stateless associated function `BTreeIndex::scan_leaf(buffer_pool,
leaf_page_id, next_after)` fresh each call, exactly mirroring how
`SeqScanExecutor::next` drives `storage::heap::TableHeap::scan_page` -
except where `scan_page` resumes from a raw page/slot pair,
`next_after` is the *sort key* of the last row this executor actually
returned (`storage::btree.MD`'s `scan_leaf`/`LeafScan` entries explain
why: a raw slot a concurrent `insert` split out from under it would
silently skip or re-yield rows, where comparing against the last row's
own sort key finds the next one correctly regardless of which physical
leaf a split has since moved it to).

Locking mirrors `seq_scan.MD`: `init` takes `ctx.lock_manager.lock_table(ctx.txn.txn_id,
table_id, LockMode::Shared)`, and each `next` that yields a row takes
`ctx.lock_manager.lock(ctx.txn.txn_id, rid, LockMode::Shared)` on that
row's `Rid` (already known from the index entry, unlike `SeqScanExecutor`
which has to construct one from `page_id`/`slot`) before fetching it from
the heap.

## Key Components

- `IndexScanExecutor` - holds `index_id`, `table_id`, the `start`/`end`
  bounds (already `types::MemcomparableEncode`-encoded bytes, produced by
  `planner::optimizer::IndexScanRule`), the target table's `column_types`
  and `table_first_page_id` (both resolved at `init` time, needed to fetch
  and decode a matching row), and the cursor `current_leaf: Option<PageId>`/
  `next_after: Option<Vec<u8>>`.
- `IndexScanExecutor::new(index_id, table_id, start, end) -> Self` -
  creates a scan over `index_id`'s `[start, end)` range (`None` on either
  end means unbounded), with the cursor and table metadata left unset
  until `init` runs.

## Usage Example

```rust
use executor::{Executor, IndexScanExecutor};

let mut scan = IndexScanExecutor::new(index_id, table_id, Some(low), Some(high));
scan.init(&mut ctx)?;
while let Some(tuple) = scan.next(&mut ctx)? {
    // one row whose indexed column falls in [low, high)
}
```

`engine::executor_factory::build_executor` constructs an
`IndexScanExecutor` for every `PhysicalPlan::IndexScan`, which only ever
exists inside a `Filter` that `planner::optimizer::IndexScanRule` built -
the `Filter` above it re-checks the full original predicate against every
row this yields, so an `IndexScanExecutor` only ever needs to narrow
candidates, never to decide membership exactly (see
`planner::optimizer.MD`). Duplicate keys are returned once per matching
`Rid`, following the B+tree leaf sibling chain across a leaf boundary the
same way `storage::btree::BTreeIndex::get`/`range_scan` do.
