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
  fall under it for as long as the open got. Opens `DiskManager` - which
  takes the database's one exclusive file lock
  (`storage::block_device::lock_exclusive`, `disk.MD`) - *before* the
  write-ahead log or the double-write buffer, deliberately: a second
  process racing to open the same database must fail on the lock before
  either of those ever touches a file. `storage::wal::LogManager::open`'s
  own bootstrap writes (`write_segment_header`'s `set_len(0)` on a
  headerless active segment, in particular) are exactly the kind of
  mutation a second, doomed-to-fail process must never be allowed to make
  against a database another process already has open. A dedicated
  whole-database lock file, taken before any of the three managers open
  anything, was the alternative on the table; opening `DiskManager` first
  and letting its own lock stand in for that was chosen instead, since it
  needs no new on-disk convention and the ordering constraint - lock
  before any WAL or double-write-buffer file is touched - is met either
  way.
- `Database::open_with_devices(config, db_device, wal_store,
  wal_segment_size, dwb_device) -> Result<Self>` - `#[cfg(any(test,
  feature = "test-util"))]`. Opens a database backed by a caller-supplied
  `db_device`/`dwb_device` (`storage::block_device::BlockDevice`) and a
  caller-supplied `wal_store` (`storage::wal::SegmentStore`) rather than
  real files, for the crash-injection harness: wrapping a device in a
  `storage::block_device::FaultyDevice`, or every segment a `SegmentStore`
  opens in one (`storage::wal::FaultySegmentStore`), lets a test simulate a
  crash at an exact point in a workload without actually killing a
  process. Takes a `SegmentStore` rather than a single `BlockDevice` for
  the log because there is exactly one on-disk WAL layout - a numbered
  segment family - and production `open_impl` builds one too
  (`docs/adr/0011-segmented-write-ahead-log.md`); a harness that wrote
  through a single-device shortcut here and then recovered through the
  real segment-family path in `open_impl` would silently be reading and
  writing two different logs. Not logged like `open` - it's test-only
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
- `Database::maybe_checkpoint(&mut self)` - private. Returns immediately,
  before even reading `bytes_at_last_checkpoint`, if
  `buffer_pool.is_flush_poisoned()` - a checkpoint's `write_checkpoint`
  ends in a flush, so once the pool is poisoned every attempt would fail,
  log a `warn!`, and leave `bytes_at_last_checkpoint` stuck, guaranteeing
  the next statement retries and fails the same way; the early return
  turns that into silence instead of one warning per statement forever.
  Otherwise checkpoints if the log has grown by `checkpoint_byte_threshold`
  bytes since the last one, bounding how much of the log a future recovery
  has to scan. Called after every statement's commit.
- `Database::execute(&mut self, sql) -> Result<ResultSet>` - the public
  entry point. Assigns this call's `stmt_id`, clones `txn_span` (a `Span`
  is cheap to clone - it's a handle, not the span's data) and enters it
  for the duration of the call so a statement that joins an explicit
  transaction nests under it, times the call, then checks
  `buffer_pool.is_flush_poisoned()` before delegating to `execute_impl` -
  a poisoned pool fails this statement immediately with
  `common::Error::FlushPoisoned` rather than letting it appear to succeed
  (an `INSERT` into an already-resident page plus a WAL commit needs no
  flush) and degrade silently until eviction finally has no clean frame
  left to reclaim. This is a fatal, terminal condition once it happens -
  reopening the database is what actually clears `flush_poisoned` - so
  every statement after the first failure gets the same clear error
  rather than the vaguer, delayed `BufferPoolExhausted`/`FlushPoisoned`
  mix a real eviction would eventually surface. On success, logs `info!`
  with `sql::fingerprint(sql)` as
  the `fingerprint` field (never the raw SQL - see CLAUDE.md's logging
  section) and, separately, a `warn!` naming `duration_ms` if the call ran
  past `slow_query_warn_threshold_ms`. On failure, logs the same
  `fingerprint` field again - the one place it matters more than on the
  success path, since it's the only clue an operator gets about *what*
  failed - alongside `error = %err.redacted()` (`common::Error::redacted`;
  see `common/src/error.MD`) at `warn` for an ordinary (`Severity::Error`)
  failure or `error` for a `Fatal`/`Panic` one, plus a `debug!`-level event
  on the same line carrying the full, unredacted `%err` so the detail a
  human deliberately turned the level up for is still there. This is the
  one place a statement's outcome is reported, per CLAUDE.md's "log at the
  boundary" rule; every layer below stays silent. `redacted()` rather than
  `Display` is deliberate: several `Error` variants embed a user-supplied
  literal (a syntax error's offending token, an out-of-range value), and
  WARN is a level every deployment runs permanently, unlike `debug`.
- `Database::execute_impl(&mut self, sql, stmt_id) -> Result<ResultSet>` -
  private. `#[tracing::instrument(skip_all, fields(stmt_id, statement_kind
  = Empty))]` - `statement_kind` isn't known until after parsing, so it
  starts empty and is filled via `Span::current().record` once the
  `Statement` exists. Logs `debug!` with the full, unredacted `sql` before
  parsing - the one place this crate is allowed to do that (see CLAUDE.md).
  Parses the statement, then dispatches: `BEGIN`/`START TRANSACTION`,
  `COMMIT`, and `ROLLBACK` go straight to their `handle_*` method (they
  reference no tables or columns, so binding is meaningless for them);
  `EXPLAIN` goes to `handle_explain`, which does its own binding rather
  than going through `execute_non_control_statement` - `EXPLAIN` needs no
  transaction at all, so it must never reach `txn_for_statement`;
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
  statement's just-begun transaction is undone and the catalog is reloaded
  (`reload_catalog`, both best-effort - if either fails there's nothing
  more useful to report than the original error, so that failure is
  swallowed) rather than left dangling for a future crash recovery to
  clean up; on failure *inside an explicit transaction*, `txn_slot`
  becomes `Aborted` instead - the transaction is not undone (or the
  catalog reloaded) yet, matching the Postgres semantics
  `common::Error::TransactionAborted` documents; that happens later, when
  `handle_commit`/`handle_rollback` actually ends it.
- `Database::handle_explain(&mut self, statement) -> Result<ResultSet>` -
  private. `EXPLAIN [VERBOSE] <inner>`: binds `statement` (the whole
  `sql::Statement::Explain`, not just `inner`) against `self.catalog`,
  unwraps the resulting `planner::BoundStatement::Explain`, then runs
  `planner::plan` -> `Optimizer::optimize` -> `to_physical` on `inner`
  exactly like `execute_bound`'s `Select` arm does, but hands the
  optimized logical plan (only under `VERBOSE`, prefixed with a `"Logical
  plan:"` header row) and the physical plan (always, prefixed with
  `"Physical plan:"` when the logical one is also shown) to
  `planner::explain_logical`/`planner::explain_physical` instead of to
  `run`. Deliberately does not go through `txn_for_statement`/
  `execute_non_control_statement` at all: no transaction is begun, no
  autocommit bookkeeping happens, and an already-open explicit
  transaction (e.g. `EXPLAIN INSERT ...` mid-`BEGIN`) is left completely
  untouched, since binding and planning only ever read the already-loaded
  `self.catalog` in memory - the buffer pool is never touched beyond that.
  Each returned line becomes one `Value::Varchar` row under a single
  `QUERY PLAN` column (`ResultSet::rows`), the conventional name a SQL
  client special-cases.
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
  first failed - undoing its writes and reloading the catalog
  (`reload_catalog`, propagating either failure this time, unlike the
  autocommit path: a `COMMIT` the caller is actively waiting on should
  report a reload failure rather than silently leaving `self.catalog`
  stale) - and returns `ResultSet::RolledBack` instead, so the caller can
  tell its `COMMIT` did the opposite of what it asked; if `txn_slot` is
  `None`, `common::Error::NoActiveTransaction`. Clears `txn_span` to
  `None` in every ending branch.
- `Database::handle_rollback(&mut self) -> Result<ResultSet>` - private.
  `ROLLBACK`: ends the active explicit transaction (healthy or already
  `Aborted` - `TxnSlot::txn_id` doesn't distinguish them here), undoing
  its writes, and reloads the catalog (`reload_catalog`).
  `common::Error::NoActiveTransaction` if `txn_slot` is `None`. Clears
  `txn_span` to `None`.
- `Database::reload_catalog(&mut self) -> Result<()>` - private. Discards
  `self.catalog` and rebuilds it from scratch via `Catalog::open`, under
  its own short-lived transaction (committed immediately - `Catalog::open`
  only ever *writes* if this is the very first open of a brand-new
  database, which can't be true here). Called after every abort -
  autocommit failure, `handle_commit`'s deferred-rollback branch, and
  `handle_rollback` - not only where the name might suggest. Needed
  because an aborted statement's WAL undo reverts durable state
  (a `CREATE TABLE`'s heap-page writes, an index's root-page row after a
  reverted B+tree split) that `self.catalog`'s in-memory maps and
  `IndexInfo`'s cached `root_page_id` (`catalog::catalog.MD`'s
  "Root-page caching") don't automatically learn about; rather than
  trying to reverse either surgically, `Catalog::open` (already able to
  rebuild both from their heaps) is simply called again, discarding the
  stale `Catalog` for one read fresh off disk - simplest thing that is
  actually correct, and the only sound way to keep a cache like
  `IndexInfo`'s from silently going stale across an abort.
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
  `catalog.create_table`; `Insert` plans straight to a `PhysicalPlan`
  (no `Optimizer` step - there is nothing in an `Insert` plan for
  `planner::optimizer::IndexScanRule` to act on) and calls `run`; `Select`
  plans, then runs `planner::Optimizer::new(vec![Box::new(IndexScanRule)])
  .optimize` against the logical plan before lowering to physical - the
  only statement kind that goes through `Optimizer` at all - then calls
  `run`; both log the physical plan at `debug!` (full plan, including any
  literal values it carries - allowed at this level per CLAUDE.md, the
  same as the raw SQL text they came from) and differ only in how the
  resulting rows are wrapped into a `ResultSet`. `CreateIndex` calls
  `catalog.create_index` then `populate_index` - like `CreateTable`, it
  never reaches `planner::plan`/`Optimizer`/`to_physical` at all, since it
  has no tuple stream to pull and nothing to plan. `Explain` hits this
  match's one `unreachable!` arm - `handle_explain` unwraps its own
  `BoundStatement::Explain` and never calls `execute_bound` with one.
- `Database::populate_index(&self, txn_id, table_id, column_index,
  index_id) -> Result<()>` - private. Scans `table_id`'s heap directly
  (`storage::heap::TableHeap::open`/`iter`, not through a `SeqScanExecutor`
  - this runs before the statement's own transaction has an
  `ExecutorContext` to hand one) and, for each row, does exactly what
  `executor::InsertExecutor` does per row when maintaining an index:
  encode the value at `column_index` via
  `types::MemcomparableEncode::encode_memcomparable` and insert it into
  `index_id`'s `storage::btree::BTreeIndex`. Resolves the index's root
  page once via `catalog.index_root_page` *before* the loop rather than
  once per row (mirroring `executor::operators::insert.MD`'s
  `InsertExecutor` - there is only one index here, but the same
  once-per-statement principle applies), tracking it in a local that's
  only updated - via `catalog.update_index_root_page` - when a row's
  insert actually splits the root. On an empty table this loop simply
  never executes - no special-casing needed. Runs under the same `txn_id`
  as the rest of the `CREATE INDEX` statement, so a later failure (or an
  explicit `ROLLBACK`) undoes the whole thing via the ordinary abort path,
  the same guarantee `executor::InsertExecutor` relies on for its own
  index-maintenance failures.
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
  Skips the flush entirely when `buffer_pool.is_flush_poisoned()`: once a
  flush has failed in the specific window `storage::buffer::BufferPool`'s
  poisoning protects (`buffer.MD`), the double-write buffer holds a batch
  recovery still needs, and a destructor doing further I/O against that
  same pool is exactly the second, uncoordinated flush poisoning exists to
  prevent - `close()` is unaffected and still attempts its flush and still
  returns the error, since an explicit close has somewhere to report a
  failure to and a destructor does not.
- `new_connection_span() -> tracing::span::EnteredSpan` - private free
  function. Allocates the next `conn_id` from `NEXT_CONN_ID`, creates and
  enters an `info_span!("connection", conn_id)`, logs `info!` "connection
  opened" (now nested under it), and returns the entered guard for its
  caller to store. Called once per `open_impl`/`open_with_devices` call,
  never per statement or transaction - it is the outermost span in the
  connection/transaction/statement hierarchy.
- `statement_kind(&Statement) -> &'static str` - private free function.
  `"SELECT"`/`"INSERT"`/`"CREATE TABLE"`/`"CREATE INDEX"`/`"BEGIN"`/
  `"COMMIT"`/`"ROLLBACK"`/`"EXPLAIN"`, used to fill `execute_impl`'s
  `statement_kind` span field.
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
