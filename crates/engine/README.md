# engine

The database facade: the single entry point that wires the SQL, planner,
executor, catalog, storage, and transaction crates together behind one
`Database::execute(sql) -> ResultSet` call.

## Architecture

Every other crate in the workspace is a layer; `engine` is where the
layers are assembled (see `docs/adr/0002-crate-splitting.md`). Nothing
below this crate knows about any other sibling — `sql` does not know about
`executor`, `catalog` does not know about `planner` — `engine` is the only
place that does; it composes the other crates rather than adding logic of
its own, which is why it defines no error type of its own (see
Dependencies) even though it does pull in `tracing` to log each error
exactly once at the boundary it owns (`Database::open`/`Database::execute`
- see CLAUDE.md's "log at the boundary" rule).

`Database::open` runs a fixed three-step sequence, in this order, and the
order is forced rather than incidental:

1. **Double-write restore** (`storage::recovery::recover_double_write`)
   repairs any page a crash left torn *before* anything else touches the
   file. Every later step reads pages through the ordinary buffer-pool
   path, which trusts a page's checksum; running this first is what makes
   that trust valid.
2. **ARIES recovery** (`storage::recovery::recover`), against the
   now-untorn file, replays every logged change (Redo, idempotent even for
   changes already on disk) and undoes anything logged by a transaction
   that never committed (Undo). This must finish before anything above
   `storage` reads a page, including the catalog's own heap — an
   in-progress `INSERT` that never committed could otherwise leave a
   half-written row visible to catalog load. `recover` also returns the
   highest `TxnId` it observed in the log, which seeds `TransactionManager`
   so a new transaction can never collide with an id recovery just
   finished undoing.
3. **Catalog load** (`Catalog::open`, under its own short-lived bootstrap
   transaction) rebuilds the in-memory table registry by replaying the
   catalog's own heap — which is only safe to trust once step 2 has
   guaranteed that heap reflects a fully redone-and-undone, consistent
   state.

`Database::execute` then runs one pipeline per statement: lex
(`sql::Lexer`) -> parse (`sql::Parser`) -> bind (`planner::Binder`) -> plan
(`planner::plan`) -> optimize (`planner::Optimizer::optimize`, `SELECT`
only) -> lower (`to_physical`) -> build an operator tree
(`executor_factory::build_executor`) -> run it (`executor::Executor::init`/
`next`), wrapped in an implicit per-statement transaction unless an
explicit `BEGIN` is already open. `CREATE TABLE`/`CREATE INDEX` are the
exception: both bind the same way but are handled directly by `execute`
rather than reaching the optimizer or the executor, since each mutates the
catalog itself rather than producing rows - `CREATE INDEX` additionally
populates the new index from every existing row
(`Database::populate_index`) before returning, under the same transaction
as the `CREATE INDEX` statement itself.

`EXPLAIN [VERBOSE] <statement>` runs the same bind -> plan -> optimize ->
lower prefix as its target statement would, then stops: the resulting
plan tree(s) are rendered by `planner::explain_logical`/`explain_physical`
and returned as rows instead of ever reaching `executor_factory`/the
executor. It never begins a transaction and never touches the buffer pool
beyond the catalog already resident in memory, so `EXPLAIN INSERT ...`
inside an open explicit transaction leaves that transaction exactly as it
was (`Database::handle_explain`, `database.MD`).

## Key Components

- `database` - `Database`, owns the catalog, buffer pool, and transaction
  manager; opens, closes, and executes SQL against a single database. See
  [database.MD](src/database.MD).
- `result_set` - `ResultSet`, the result of executing one SQL statement.
  See [result_set.MD](src/result_set.MD).
- `executor_factory` - `build_executor`, lowers a `planner::PhysicalPlan`
  into an `executor` operator tree. Private to the crate. See
  [executor_factory.MD](src/executor_factory.MD).

`DataType`, `Tuple`, and `Value` are also part of this crate's public API,
re-exported from `types` rather than duplicated, so `cli` — which may
depend only on `engine` and `common` per the workspace's dependency-edge
rules — can name the types `ResultSet`'s rows are made of without
depending on `types` directly.

## Features

`CREATE TABLE`, `CREATE INDEX`, `INSERT`, `SELECT` (choosing an index scan
over a sequential scan where `planner::optimizer::IndexScanRule` applies),
`BEGIN`/`COMMIT`/`ROLLBACK`, and `EXPLAIN [VERBOSE]` (of any statement
`planner::plan` can handle - not a transaction-control statement or
another `EXPLAIN`) all work end to end today, durable across a restart
and atomic per transaction (`docs/adr/0004-acid-scope.md`). Crash
recovery and torn-page repair run on every open and are swept exhaustively
by `tests/crash_injection.rs` across every fail point and every
`storage::block_device::DurabilityModel`; `tests/index_equivalence.rs`
separately sweeps randomized tables/predicates comparing indexed and
sequential-scan results for equality, and `tests/index_scan.rs` checks an
index's durability across a root split and a restart, `NULL` handling, and
populating an index created on a non-empty table. What's missing is
entirely inherited from the layers `engine` assembles, not added here: no
`DROP`/`UPDATE`/`DELETE`/`JOIN` (no crate above `sql` supports them yet),
no concurrent transactions (`txn`'s lock manager and MVCC are unwired,
M10), no composite/multi-column indexes, and no cost-based optimization
choosing *among* multiple viable access paths (M11 - `IndexScanRule` picks
an index whenever one applies, but has no cost model for picking among
several). See `docs/ROADMAP.md`.

## Dependencies

Workspace: `common`, `types`, `storage`, `catalog`, `sql`, `txn`,
`planner`, `executor` — `engine` composes the layers below it and reuses
`common::Error` rather than defining its own error type, since assembling
the other crates never needs a new error variant. External: `tracing`,
for the one log line each `Database::open`/`execute` emits when it returns
an error - the only external dependency this crate needs, since logging
that once at the boundary is itself boundary-assembly, not domain logic.
Dev-only: this crate again with its `test-util` feature enabled (for
`Database::open_with_devices`, used by the crash-injection harness to open
against fault-injecting `BlockDevice`s and a fault-injecting
`storage::wal::SegmentStore` instead of real files), plus
`tempfile` and `proptest` (for `tests/index_equivalence.rs`'s randomized
index-vs-sequential-scan comparison).

## Configuration

`engine` has no configuration constants of its own beyond
`REPLACER_K = 2` (private, the buffer pool's LRU-K eviction parameter).
Every sizing knob a `Database` is opened with — page size, buffer pool
size, checkpoint byte threshold, double-write buffer capacity, slow-query
warn threshold — comes from `common::DbConfig`, constructed by the caller
(`Database::open`'s only argument) and threaded straight down into the
disk manager, double-write buffer, buffer pool, and `Database::execute`'s
own logging. Log output itself is controlled by `RUST_LOG` (`cli` wires up
the subscriber; see CLAUDE.md's logging section), not by `DbConfig`.

## Testing

`tests/integration.rs` runs the full lex -> parse -> bind -> plan ->
execute pipeline against a real (tempfile-backed) database file.
`tests/index_scan.rs` and `tests/index_equivalence.rs` are `CREATE
INDEX`/index-scan-specific: the former deterministic (root split survives
a restart, `NULL` handling, populating a non-empty table, a rolled-back
`CREATE INDEX`), the latter a seeded proptest comparing indexed and
sequential-scan query results for equality across randomized data and
predicates - the test that matters most for catching a wrong-rows bug in
`executor::IndexScanExecutor` or `planner::optimizer::IndexScanRule`.
`tests/transactions.rs` checks `BEGIN`/`COMMIT`/`ROLLBACK` group statements
atomically. `tests/rollback_matches_recovery_undo.rs` proves
`TransactionManager::abort` and `storage::recovery::recover`'s Undo pass
are one mechanism. `tests/crash_injection.rs` is the crash-injection
harness described in Features. `tests/smoke.rs` is the minimum-viable
compile-and-construct check. Run just this crate with:

```sh
cargo test -p engine
```
