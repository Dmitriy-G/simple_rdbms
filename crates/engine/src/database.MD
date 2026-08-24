# database

A single open database. Owns the catalog, buffer pool, and transaction
manager, and is the object every SQL statement is executed against.

`Database` is also where the workspace's structured-logging correlation
lives (see CLAUDE.md's logging section): opening one enters a connection
span for the process's whole session with it, `execute` opens one
statement span per call, and an open explicit transaction's own span sits
between them, so every `tracing` event emitted anywhere below - a page
fetch three crates down, an evicted frame, a WAL append - automatically
carries `conn_id`, and, when relevant, `txn_id`/`stmt_id`, without any of
those lower layers needing to know a connection or a transaction exists.

## Key Components

- `REPLACER_K` - the number of recent accesses the buffer pool's
  `LruKReplacer` tracks per frame before falling back to plain LRU. Not
  yet exposed as a config knob; `2` is the classic LRU-2 choice.
- `NEXT_CONN_ID` - private, a process-wide `AtomicU64` handing out fresh
  connection ids. There is no real client-server connection concept yet
  (see `docs/ROADMAP.md`); this stands in for one until a server binary
  exists, letting the span hierarchy below be written once now instead of
  retrofitted later.
- `TxnSlot` - private. The explicit-transaction state machine: `None` (no
  `BEGIN` open - every statement runs autocommit, begun, committed, and,
  on failure, aborted, all within one `execute` call), `Active(TxnId)` (an
  explicit transaction is open and healthy - the next statement joins it),
  or `Aborted(TxnId)` (an explicit transaction is open but a statement
  inside it already failed - every statement until `ROLLBACK` is now
  rejected with `common::Error::TransactionAborted` rather than being run,
  matching the Postgres behavior every SQL client already expects).
  `TxnSlot::txn_id(self) -> Option<TxnId>` - private. The id either
  transaction-holding variant carries, regardless of whether it's healthy
  or aborted; used by `handle_begin` (nesting is rejected the same way
  whether or not the existing transaction is aborted) and
  `handle_rollback` (a `ROLLBACK` ends an aborted transaction exactly like
  a healthy one).
- `Database` - holds `catalog`, `buffer_pool`, `txn_manager`,
  `checkpoint_byte_threshold` (the trigger for `write_checkpoint`: once
  `buffer_pool.log_bytes_appended()` has grown past this many bytes beyond
  the value recorded at the last checkpoint), `bytes_at_last_checkpoint`
  (`buffer_pool.log_bytes_appended()`'s value as of the last checkpoint,
  or database open if none has happened yet this session),
  `slow_query_warn_threshold_ms` (from `DbConfig`, the duration past which
  `execute` logs a statement at `warn` instead of only `debug`),
  `txn_slot: TxnSlot` (the state described above), `txn_span:
  Option<tracing::Span>` (the current explicit transaction's span, entered
  around every statement that joins it - `None` under autocommit),
  `next_stmt_id` (a per-connection counter, not process-wide like
  `NEXT_CONN_ID`, since a statement only ever needs to be unique within
  its own connection's log), and `_conn_span:
  tracing::span::EnteredSpan` (entered once, in `new_connection_span`, and
  held for this `Database`'s entire lifetime - see below).
- `Database::open(config) -> Result<Self>` - the public entry point;
  delegates to `open_impl` and logs any error it returns (`sql_state` and
  the error itself) exactly once before propagating it - the one place a
  startup failure is reported, per CLAUDE.md's "log at the boundary" rule.
- `Database::open_impl(config) -> Result<Self>` - private. Opens (creating
  if necessary) the database described by `config`, standing up its disk
  manager and buffer pool, running crash recovery against whatever the
  write-ahead log describes, and only then loading the catalog - the
  pages the catalog reads must already be consistent before anything
  above `storage` starts trusting them. Calls `new_connection_span` as its
  very first action, before anything that could fail, so a startup
  failure's own `error!` log (from `open`, above) still has the chance to
  fall under it for as long as the open got.
- `Database::open_with_devices(config, db_device, wal_device,
  dwb_device) -> Result<Self>` - `#[cfg(any(test, feature =
  "test-util"))]`. Opens a database backed by caller-supplied
  `BlockDevice`s rather than real files, for the crash-injection harness:
  wrapping a device in a `storage::block_device::FaultyDevice` lets a test
  simulate a crash at an exact point in a workload without actually
  killing a process. Not logged like `open` - it's test-only
  infrastructure, not a boundary a real caller ever crosses - but still
  opens its own connection span the same way, so span-hierarchy tests can
  exercise the real thing.
- `Database::open_with_managers(config, disk_manager, dwb, log_manager,
  conn_span) -> Result<Self>` - private. Shared tail of `open_impl`/
  `open_with_devices`: repairs any real-file page a crash tore mid-write
  from the double-write buffer (must run before recovery trusts page
  contents, and before any `BufferPool` exists to run it against), stands
  up the buffer pool over the already-constructed managers, runs ARIES
  recovery, and loads the catalog. The catalog's own bootstrap heap
  allocation (only ever happens on a brand-new database) needs a real,
  committed transaction of its own - otherwise it would be a dangling,
  never-committed write that the next recovery would undo as a loser.
  Takes ownership of `conn_span` (created by its caller) and stores it on
  the returned `Database`. Emits a single `info!` "database opened" event
  just before returning: `page_size`, `buffer_pool_frames`,
  `dwb_capacity`, `checksums_enabled` (always `true` today - there is no
  toggle, but the field is worth emitting anyway per CLAUDE.md's logging
  section: "the startup line is the most valuable one in the whole
  system"), and `recovered_highest_txn_id` (what `recovery::recover`
  returned) - together, everything someone would need to answer "was a
  lost write possible" after the fact.
- `Database::close(self) -> Result<()>` - closes the database: writes a
  final checkpoint, forces every write-ahead log record to disk, flushes
  every dirty page, and syncs the file to durable storage, then logs
  `info!` "connection closed" - still nested under `_conn_span`, since
  `self` (and the span it owns) isn't dropped until this function returns.
  Errors here are real failures worth reporting, which is why this is a
  separate, explicit call rather than left entirely to `Drop` (which
  cannot report them, and does not log a matching close event either).
- `Database::maybe_checkpoint(&mut self) -> Result<()>` - private.
  Checkpoints if the log has grown by `checkpoint_byte_threshold` bytes
  since the last one, bounding how much of the log a future recovery has
  to scan. Called after every statement's commit.
- `Database::execute(&mut self, sql) -> Result<ResultSet>` - the public
  entry point. Assigns this call's `stmt_id`, clones `txn_span` (a `Span`
  is cheap to clone - it's a handle, not the span's data) and enters it
  for the duration of the call so a statement that joins an explicit
  transaction nests under it, times the call, then delegates to
  `execute_impl`. On success, logs `info!` with `sql::fingerprint(sql)` as
  the `fingerprint` field (never the raw SQL - see CLAUDE.md's logging
  section) and, separately, a `warn!` naming `duration_ms` if the call ran
  past `slow_query_warn_threshold_ms`. On failure, logs at `warn` for an
  ordinary (`Severity::Error`) failure or `error` for a `Fatal`/`Panic`
  one - this is the one place a statement's outcome is reported, per
  CLAUDE.md's "log at the boundary" rule; every layer below stays silent.
- `Database::execute_impl(&mut self, sql, stmt_id) -> Result<ResultSet>` -
  private. `#[tracing::instrument(skip_all, fields(stmt_id, statement_kind
  = Empty))]` - `statement_kind` isn't known until after parsing, so it
  starts empty and is filled via `Span::current().record` once the
  `Statement` exists. Logs `debug!` with the full, unredacted `sql` before
  parsing - the one place this crate is allowed to do that (see CLAUDE.md).
  Parses the statement, then dispatches: `BEGIN`/`START TRANSACTION`,
  `COMMIT`, and `ROLLBACK` go straight to their `handle_*` method (they
  reference no tables or columns, so binding is meaningless for them);
  everything else goes to `execute_non_control_statement`. Times just the
  dispatch call (not parsing) and records it into the
  `query_duration_seconds` histogram labeled `statement_kind` - never by
  statement text, which would give Prometheus unbounded label cardinality
  (see `docs/ROADMAP.md`'s M13 entry) - regardless of whether the
  statement succeeded or failed.
- `Database::execute_non_control_statement(&mut self, statement) -> Result<ResultSet>`
  - private. The part of the old single-function dispatch that isn't
  `BEGIN`/`COMMIT`/`ROLLBACK`, split out so `execute_impl` can time
  *only* this call for `query_duration_seconds` without also timing
  parsing. Asks `txn_for_statement` for the transaction to run under
  (which rejects it outright if `txn_slot` is `Aborted`), then binds and
  executes the statement as one fallible step. On autocommit failure the
  statement's just-begun transaction is undone rather than left dangling
  for a future crash recovery to clean up (if the abort itself fails
  there's nothing more useful to report than the original error, so that
  failure is swallowed); on failure *inside an explicit transaction*,
  `txn_slot` becomes `Aborted` instead - the transaction is not undone
  yet, matching the Postgres semantics `common::Error::TransactionAborted`
  documents.
- `Database::handle_begin(&mut self) -> Result<ResultSet>` - private.
  `BEGIN`/`START TRANSACTION`: opens an explicit transaction and creates
  its `txn_span` (`txn_id`, `isolation` fields) - a plain `Span`, not
  entered here; `execute` enters a clone of it around every later
  statement that joins this transaction, which is what makes the
  connection-span-then-transaction-span-then-statement-span nesting real
  across multiple separate `execute` calls rather than just within one.
  Nesting - including `BEGIN` while `txn_slot` is `Aborted` - is
  `common::Error::NestedTransaction`.
- `Database::handle_commit(&mut self) -> Result<ResultSet>` - private.
  `COMMIT`: if `txn_slot` is `Active`, ends the transaction keeping its
  writes and returns `ResultSet::rows_affected(0)`; if `txn_slot` is
  `Aborted`, performs the rollback that was deferred when the transaction
  first failed and returns `ResultSet::RolledBack` instead, so the caller
  can tell its `COMMIT` did the opposite of what it asked; if `txn_slot`
  is `None`, `common::Error::NoActiveTransaction`. Clears `txn_span` to
  `None` in every ending branch.
- `Database::handle_rollback(&mut self) -> Result<ResultSet>` - private.
  `ROLLBACK`: ends the active explicit transaction (healthy or already
  `Aborted` - `TxnSlot::txn_id` doesn't distinguish them here), undoing
  its writes, and reloads the catalog from its heap.
  `common::Error::NoActiveTransaction` if `txn_slot` is `None`. Clears
  `txn_span` to `None`. The reload is needed because a `CREATE TABLE`
  inside the rolled-back transaction has its heap-page writes undone by
  `abort` but the in-memory `tables_by_name`/`tables_by_id` maps don't
  know that; rather than trying to reverse them surgically, `Catalog::open`
  (already able to rebuild them from an existing heap) is simply called
  again, discarding the stale `Catalog` for one read fresh off disk -
  simplest thing that is actually correct.
- `Database::txn_for_statement(&mut self) -> Result<(TxnId, bool)>` -
  private. The transaction the next statement should run under: the id
  from `txn_slot` if `Active` (`autocommit = false`), a fresh one if
  `txn_slot` is `None` (`autocommit = true`, meaning `execute_impl` itself
  must commit or abort it once the statement is done), or
  `common::Error::TransactionAborted` if `txn_slot` is `Aborted` - the
  point where a statement issued after one has already failed gets turned
  away without ever reaching the binder. An autocommit transaction's own
  `txn_id`/`isolation` still end up in the log via
  `TransactionManager::begin`'s own `#[instrument]` span, just nested
  *under* that one statement's span rather than wrapping it - there is
  only ever one statement to correlate, so nothing is lost by not
  building a separate long-lived `txn_span` for it the way an explicit
  transaction gets one.
- `Database::execute_bound(&mut self, bound, txn_id) -> Result<ResultSet>`
  - private. Runs a bound, non-transaction-control statement under
  `txn_id`: `CreateTable` builds a `Schema` and calls
  `catalog.create_table`; `Insert` and `Select` both plan to a
  `PhysicalPlan`, log it at `debug!` (full plan, including any literal
  values it carries - allowed at this level per CLAUDE.md, the same as the
  raw SQL text they came from), and call `run`, differing in how the
  resulting rows are wrapped into a `ResultSet`.
- `Database::table_names(&self) -> Vec<String>` - every registered
  table's name, sorted for a deterministic order.
- `Database::table_schema(&self, name) -> Result<Schema>` - the column
  schema of the table named `name`.
- `Database::run(&self, physical, txn_id) -> Result<Vec<Tuple>>` -
  private. Builds and drains an executor tree for `physical` under
  `txn_id`, collecting every tuple it produces. Never logs the rows
  themselves at any level - see CLAUDE.md's logging section: statement
  *text* and its literals are a `debug` concern, but result *data* is
  never logged, full stop.
- `impl Drop for Database` - best-effort flush on drop, for callers that
  don't call `close` explicitly. Errors are swallowed here (there is
  nowhere to report them from `Drop`); call `close` to observe them. Does
  not log a "connection closed" event, matching that same asymmetry.
- `new_connection_span() -> tracing::span::EnteredSpan` - private free
  function. Allocates the next `conn_id` from `NEXT_CONN_ID`, creates and
  enters an `info_span!("connection", conn_id)`, logs `info!` "connection
  opened" (now nested under it), and returns the entered guard for its
  caller to store. Called once per `open_impl`/`open_with_devices` call,
  never per statement or transaction - it is the outermost span in the
  connection/transaction/statement hierarchy.
- `statement_kind(&Statement) -> &'static str` - private free function.
  `"SELECT"`/`"INSERT"`/`"CREATE TABLE"`/`"BEGIN"`/`"COMMIT"`/`"ROLLBACK"`,
  used to fill `execute_impl`'s `statement_kind` span field.
- `syntax_error(err, sql) -> Error` - private free function. Builds
  `common::Error::Syntax { message, offset }` from a `sql::SqlError`:
  `message` is `err.render(sql)` (the error plus the offending line and a
  caret), `offset` is `err.offset(sql)`. Both need the original source
  text, which only `execute_impl` has, which is why this isn't a `From`
  impl in `sql` itself - see `sql::error.MD`.

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
holds. A single connection span therefore covers a whole REPL session
today - there is no way yet to close one connection and open another
within the same process without the second nesting under the first if the
first is still held open, since span entry is a thread-local stack, not a
per-`Database` one; this doesn't matter for `cli` (one `Database` for the
whole process) but is worth knowing before reusing this pattern in a
future multi-connection server.
