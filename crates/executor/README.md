# executor

The query executor: turns a `planner::PhysicalPlan` into a tree of
pull-based (Volcano-model) operators and drives it to produce
`types::Tuple`s.

## Architecture

`executor` depends on `common`, `types`, `catalog`, `storage`, `txn`, and
`planner`, and sits directly below `engine`, the only crate that
constructs or drives one (see `docs/adr/0002-crate-splitting.md`). Every
operator implements the `Executor` trait: a caller calls `init` once, then
calls `next` repeatedly until it returns `Ok(None)`. Each operator pulls
from its children the same way, so the whole tree advances one tuple at a
time without materializing intermediate results — a filter over a
sequential scan, for instance, never holds more than the one row it's
currently deciding whether to pass through.

`ExecutorContext` carries the shared state (catalog, buffer pool,
transaction) every operator needs while executing, threaded through every
`init`/`next` call rather than captured at construction, so the same
operator tree shape could in principle be reused across transactions.

## Key Components

- `context` - `ExecutorContext`, the shared state every operator needs
  while executing. See [context.MD](src/context.MD).
- `error` - `ExecutorError`, errors raised while executing a physical
  plan. See [error.MD](src/error.MD).
- `executor` - `Executor`, the pull-based trait every operator implements.
  See [executor.MD](src/executor.MD).
- `expression` - `evaluate`, evaluates a bound expression against a
  tuple. See [expression.MD](src/expression.MD).
- `operators` - `FilterExecutor`, `IndexScanExecutor`, `InsertExecutor`,
  `NestedLoopJoinExecutor`, `ProjectionExecutor`, `SeqScanExecutor`: the
  concrete operators, one per physical plan node kind. See
  [operators/mod.MD](src/operators/mod.MD).

## Features

`SeqScanExecutor`, `IndexScanExecutor`, `FilterExecutor`,
`ProjectionExecutor`, and `InsertExecutor` all work today and are what
every `CREATE TABLE`/`CREATE INDEX`/`INSERT`/`SELECT` in the REPL actually
runs through. `SeqScanExecutor`/`IndexScanExecutor` are both lazy and
page-at-a-time — neither materializes its table/index before yielding its
first row (`tests/seq_scan.rs`, `tests/index_scan.rs`). `InsertExecutor`
maintains every index on its target table as it inserts
(`tests/index_maintenance.rs`).

`NestedLoopJoinExecutor::init`/`next` are both `todo!()`. Nothing
constructs one outside of tests, since `sql` has no `JOIN` syntax for a
statement to reach it through (see `planner`'s README) — it exists as
scaffolding for roadmap milestone M11, which is also where a real
cost-based choice among join algorithms would come from. See
`docs/ROADMAP.md`.

## Dependencies

Workspace: `common`, `types`, `catalog`, `storage`, `txn`, `planner`.
External: `thiserror`, for `ExecutorError`. Dev-only: `storage` again, with
its `test-util` feature enabled, plus `tempfile`.

## Configuration

None — `executor` has no configuration of its own; it's handed an
already-built `ExecutorContext` by its caller.

## Testing

`tests/seq_scan.rs` checks `SeqScanExecutor`'s lazy, page-at-a-time cursor:
tuples come back in storage order without materializing the whole table.
`tests/index_maintenance.rs` checks `InsertExecutor` maintains a single
index and every index on a multi-indexed table, and that an unorderable
indexed value (NaN) fails the statement without silently completing that
row's index entry. `tests/index_scan.rs` checks `IndexScanExecutor`
against bounded and unbounded ranges, duplicate keys, and that every row
is still reachable through the index after enough inserts to force a
B+tree root split, proving `InsertExecutor` actually persists a changed
root page rather than leaving the catalog pointing at a stale one. Both files
declare `mod support;` for shared setup helpers (`tests/support/mod.MD`).
`tests/smoke.rs` is the minimum-viable compile-and-construct check. Run
just this crate with:

```sh
cargo test -p executor
```
