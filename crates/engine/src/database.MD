# database

A single open database. Owns the catalog, buffer pool, and transaction
manager, and is the object every SQL statement is executed against.

## Key Components

- `REPLACER_K` - the number of recent accesses the buffer pool's
  `LruKReplacer` tracks per frame before falling back to plain LRU. Not
  yet exposed as a config knob; `2` is the classic LRU-2 choice.
- `Database` - holds `catalog`, `buffer_pool`, `txn_manager`,
  `checkpoint_byte_threshold` (the trigger for `write_checkpoint`: once
  `buffer_pool.log_bytes_appended()` has grown past this many bytes beyond
  the value recorded at the last checkpoint), `bytes_at_last_checkpoint`
  (`buffer_pool.log_bytes_appended()`'s value as of the last checkpoint,
  or database open if none has happened yet this session), and
  `active_txn: Option<TxnId>` (the id of the explicit transaction opened
  by an unmatched `BEGIN`, if any - `None` means every statement runs
  autocommit: begin, execute, commit, all within `execute` itself;
  `Some` means the next statement joins this transaction instead, and
  only `COMMIT`/`ROLLBACK` end it).
- `Database::open(config) -> Result<Self>` - opens (creating if
  necessary) the database described by `config`, standing up its disk
  manager and buffer pool, running crash recovery against whatever the
  write-ahead log describes, and only then loading the catalog - the
  pages the catalog reads must already be consistent before anything
  above `storage` starts trusting them.
- `Database::open_with_devices(config, db_device, wal_device,
  dwb_device) -> Result<Self>` - `#[cfg(any(test, feature =
  "test-util"))]`. Opens a database backed by caller-supplied
  `BlockDevice`s rather than real files, for the crash-injection harness:
  wrapping a device in a `storage::block_device::FaultyDevice` lets a test
  simulate a crash at an exact point in a workload without actually
  killing a process.
- `Database::open_with_managers(config, disk_manager, dwb, log_manager)
  -> Result<Self>` - private. Shared tail of `open`/`open_with_devices`:
  repairs any real-file page a crash tore mid-write from the double-write
  buffer (must run before recovery trusts page contents, and before any
  `BufferPool` exists to run it against), stands up the buffer pool over
  the already-constructed managers, runs ARIES recovery, and loads the
  catalog. The catalog's own bootstrap heap allocation (only ever happens
  on a brand-new database) needs a real, committed transaction of its
  own - otherwise it would be a dangling, never-committed write that the
  next recovery would undo as a loser.
- `Database::close(self) -> Result<()>` - closes the database: writes a
  final checkpoint, forces every write-ahead log record to disk, flushes
  every dirty page, and syncs the file to durable storage. Errors here are
  real failures worth reporting, which is why this is a separate, explicit
  call rather than left entirely to `Drop` (which cannot report them).
- `Database::maybe_checkpoint(&mut self) -> Result<()>` - private.
  Checkpoints if the log has grown by `checkpoint_byte_threshold` bytes
  since the last one, bounding how much of the log a future recovery has
  to scan. Called after every statement's commit.
- `Database::execute(&mut self, sql) -> Result<ResultSet>` - parses,
  binds, plans, and executes one SQL statement, returning its result set.
  `BEGIN`/`START TRANSACTION`, `COMMIT`, and `ROLLBACK` are handled
  directly here, before binding - they reference no tables or columns, so
  binding is meaningless for them. Every other statement runs under
  `active_txn` if one is open (joining an explicit transaction), or under
  a fresh transaction begun and committed (or, on failure, aborted) right
  here otherwise (autocommit). On autocommit failure the statement's
  just-begun transaction is undone rather than left dangling for a future
  crash recovery to clean up; if the abort itself fails there's nothing
  more useful to report than the original error, so that failure is
  swallowed.
- `Database::handle_begin(&mut self) -> Result<ResultSet>` - private.
  `BEGIN`/`START TRANSACTION`: opens an explicit transaction. Nesting is
  an error.
- `Database::handle_commit(&mut self) -> Result<ResultSet>` - private.
  `COMMIT`: ends the active explicit transaction, keeping its writes. An
  error with no active transaction.
- `Database::handle_rollback(&mut self) -> Result<ResultSet>` - private.
  `ROLLBACK`: ends the active explicit transaction, undoing its writes,
  and reloads the catalog from its heap. An error with no active
  transaction. The reload is needed because a `CREATE TABLE` inside the
  aborted transaction has its heap-page writes undone by `abort` but the
  in-memory `tables_by_name`/`tables_by_id` maps don't know that; rather
  than trying to reverse them surgically, `Catalog::open` (already able to
  rebuild them from an existing heap) is simply called again, discarding
  the stale `Catalog` for one read fresh off disk - simplest thing that is
  actually correct.
- `Database::txn_for_statement(&mut self) -> Result<(TxnId, bool)>` -
  private. The transaction the next statement should run under:
  `active_txn` if an explicit transaction is open (`autocommit = false`),
  or a freshly begun one otherwise (`autocommit = true`, meaning
  `execute` itself must commit or abort it once the statement is done).
- `Database::execute_bound(&mut self, bound, txn_id) -> Result<ResultSet>`
  - private. Runs a bound, non-transaction-control statement under
  `txn_id`: `CreateTable` builds a `Schema` and calls
  `catalog.create_table`; `Insert` and `Select` both plan to a
  `PhysicalPlan` and call `run`, differing in how the resulting rows are
  wrapped into a `ResultSet`.
- `Database::table_names(&self) -> Vec<String>` - every registered
  table's name, sorted for a deterministic order.
- `Database::table_schema(&self, name) -> Result<Schema>` - the column
  schema of the table named `name`.
- `Database::run(&self, physical, txn_id) -> Result<Vec<Tuple>>` -
  private. Builds and drains an executor tree for `physical` under
  `txn_id`, collecting every tuple it produces.
- `impl Drop for Database` - best-effort flush on drop, for callers that
  don't call `close` explicitly. Errors are swallowed here (there is
  nowhere to report them from `Drop`); call `close` to observe them.

## Usage Example

```rust
use common::DbConfig;
use engine::Database;

let mut db = Database::open(DbConfig::new("example.db"))?;
db.execute("CREATE TABLE t (a INTEGER)")?;
db.execute("BEGIN")?;
db.execute("INSERT INTO t VALUES (1)")?;
db.execute("COMMIT")?;
db.close()?;
```

`cli` opens exactly one `Database` per process and calls `execute` once
per statement the REPL reads; `crates/engine/tests/*` drive the same
public API against real and fault-injecting devices to prove the
transaction, recovery, and checkpoint behavior documented above actually
holds.
