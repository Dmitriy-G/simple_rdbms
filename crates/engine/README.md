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
(`planner::plan`/`to_physical`) -> build an operator tree
(`executor_factory::build_executor`) -> run it (`executor::Executor::init`/
`next`), wrapped in an implicit per-statement transaction unless an
explicit `BEGIN` is already open. `CREATE TABLE` is the one exception: it
binds and lowers the same way but is handled directly by `execute` rather
than reaching the executor, since it mutates the catalog itself rather than
producing rows.

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

`CREATE TABLE`, `INSERT`, `SELECT`, and `BEGIN`/`COMMIT`/`ROLLBACK` all
work end to end today, durable across a restart and atomic per transaction
(`docs/adr/0004-acid-scope.md`). Crash recovery and torn-page repair run on
every open and are swept exhaustively by
`tests/crash_injection.rs` across every fail point and every
`storage::block_device::DurabilityModel`. What's missing is entirely
inherited from the layers `engine` assembles, not added here: no index
(`storage`'s B+tree, M9), no `DROP`/`UPDATE`/`DELETE`/`JOIN` (no crate
above `sql` supports them yet), no concurrent transactions (`txn`'s lock
manager and MVCC are unwired, M10), and no cost-based optimization
(`planner`'s optimizer is `todo!()`, M11). See `docs/ROADMAP.md`.

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
against fault-injecting `BlockDevice`s instead of real files), plus
`tempfile`.

## Configuration

`engine` has no configuration constants of its own beyond
`REPLACER_K = 2` (private, the buffer pool's LRU-K eviction parameter).
Every sizing knob a `Database` is opened with — page size, buffer pool
size, checkpoint byte threshold, double-write buffer capacity — comes from
`common::DbConfig`, constructed by the caller (`Database::open`'s only
argument) and threaded straight down into the disk manager, double-write
buffer, and buffer pool.

## Testing

`tests/integration.rs` runs the full lex -> parse -> bind -> plan ->
execute pipeline against a real (tempfile-backed) database file.
`tests/transactions.rs` checks `BEGIN`/`COMMIT`/`ROLLBACK` group statements
atomically. `tests/rollback_matches_recovery_undo.rs` proves
`TransactionManager::abort` and `storage::recovery::recover`'s Undo pass
are one mechanism. `tests/crash_injection.rs` is the crash-injection
harness described in Features. `tests/smoke.rs` is the minimum-viable
compile-and-construct check. Run just this crate with:

```sh
cargo test -p engine
```
