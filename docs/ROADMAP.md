# Roadmap

Each milestone is named after the database problem it solves, not the
feature it adds — the feature is just how the problem gets solved this
time.

**The number is the priority**, in natural numeric order. The file reads
top to bottom and that is the order the work happens in. There is no
second ordering and no note explaining why an entry sits where it does:
if a milestone should happen sooner, it is renumbered.

**Each milestone is one self-contained unit of work.** Its entry says
what problem it solves and how, and everything it needs to be started
against is either already shipped or inside the milestone itself. Work
that has to be done in sequence is one milestone with sub-milestones, not
several entries spread down the file — M23 is the worked example, holding
joins, the statistics that make a join plan choosable, and the ordering
that uses them.

The numbers run 1, 2, 3 with no gaps. Removing a milestone renumbers
every one after it, and so does inserting one; a number is a position in
a list, not a name a milestone keeps. The cost is that whatever cites a
milestone — a `.MD` file, a `// TODO(Mx):` marker, `CLAUDE.md` — moves
with it, so renumbering and updating those references are one change,
never two.

Each heading carries one of three statuses, and each is set by exactly
one role:

- **🆕 New** — not started. The Architect sets it when the entry is
  written.
- **🚧 In Progress** — started. The Task writer sets it when it writes
  the milestone's first task. On a sub-milestone this means someone is
  writing code for it right now, and at most one sub-milestone across the
  roadmap carries it. On a parent it means partly delivered, so several
  parents can carry it at once.
- **✅ Done** — the Milestone Reviewer sets it, and nobody else. It
  asserts that the milestone's functionality was reviewed and works, not
  that its subtasks were all completed. A parent becomes Done only when
  everything under it is.

## M1 — Durable, fixed-size storage ✅ Done
**Problem:** a database needs a way to persist bytes to disk in units the
rest of the engine can reason about, and to pack many variable-sized rows
into those units without wasting space or losing track of where each one
is.
**Solution:** a disk manager that reads and writes fixed-size pages, and a
slotted-page layout for packing variable-length tuples into a page with a
stable per-tuple address (`Rid`).

## M2 — Bounded memory over an unbounded database ✅ Done
**Problem:** the database file will not fit in memory, but every operator
needs page-granular access as if it did, without each caller managing raw
I/O or accidentally evicting a page another caller is actively using.
**Solution:** a buffer pool that caches a fixed number of pages, pins pages
in use, and evicts unpinned pages by a replacement policy (LRU-K) when
space is needed.

## M3 — Something to query, before there's a query language ✅ Done
**Problem:** prove the storage engine can actually hold a table and answer
a query end to end, without yet paying for a parser.
**Solution:** a catalog holding table/column metadata, wired directly to a
hardcoded `CREATE`/`INSERT`/`SELECT` path.

## M4 — Accepting SQL text instead of hardcoded calls ✅ Done
**Problem:** users write SQL, not Rust function calls; the engine needs to
turn arbitrary query text into the same execution path M3 proved out.
**Solution:** a lexer and hand-written recursive-descent parser producing
an AST, and a binder that resolves that AST against the catalog, replacing
the hardcoded M3 path.

## M5 — Durable allocation, before the WAL exists ✅ Done
**Problem:** two concrete data-loss windows exist ahead of a real WAL (M6).
First, `DiskManager::allocate_page` used to write a `page_count` into the
page-0 header immediately and durably while the new page's own
initialization stayed dirty in the buffer pool; a kill between the two left
the file structurally claiming a page whose contents were still zeros.
Second, dirty pages only ever reached disk in `Database::close` and its
`Drop` impl, neither of which runs on a `SIGINT` or a hard kill — so
everything written since open was lost, not just the page in flight.
**Solution:** stop storing `page_count` in the header at all; derive
`next_page_id` at open from the file's own length
(`file_len / page_size`), erroring with a clear message if that length
isn't a whole multiple of the page size. `set_len` becomes the single
durable act of allocation — `allocate_page` no longer rewrites the header,
which also removes a 4KB write from every page allocation. At the time,
`Database::sync` (`flush_all` then `sync`) ran at the end of every
mutating statement (`CREATE TABLE`, `INSERT`), so a statement the caller
had already seen acknowledged was durable before the next one started —
a stopgap that made the pre-WAL engine honest, not the final design (see
ADR 0003 for why the real fix, the WAL, comes before the B+tree index
rather than after it). **Superseded by M6:** per-statement `flush_all`
then `sync` of every dirty page is gone; `TransactionManager::commit` now
fsyncs only the WAL's own commit record (`BufferPool::flush_log`), not the
data pages, and `Database::sync` no longer exists as a method —
`flush_all`/`sync` today run only in `Database::close`, its `Drop` impl,
and checkpointing.

## M6 — Making every write atomic and durable ✅ Done
**Problem:** M5 makes a *single* statement's pages durable by the time it's
acknowledged, but says nothing about a statement (or group of statements)
that touches multiple pages: a crash partway through can still leave some
of those pages reflecting the old state and some the new, which is a
different failure mode than the file-truncation M5 closes. There is also no
record of *what* a write changed, which any undo (M8) or redo-after-crash
(M7) needs.
**Solution:** a write-ahead log: every page mutation is described by a log
record appended (and synced) before the page itself is allowed to reach
disk, plus periodic checkpointing so recovery after a crash only has to
scan back to the last checkpoint instead of the log's entire history. The
log is a family of numbered segment files, not one ever-growing file
(`docs/adr/0011-segmented-write-ahead-log.md`): `write_checkpoint` deletes
every sealed segment nothing undo or redo can still reach, so the log's
on-disk size and a reopen's startup cost both stay bounded by activity
since the last checkpoint rather than by the database's total lifetime.

## M7 — Recovering from a crash ✅ Done
**Problem:** the WAL (M6) only helps if something replays it. On restart
after a crash, the buffer pool is empty and the disk holds whatever mix of
old and new page images the crash left behind; the engine needs to bring
that back to a consistent state before accepting new statements.
**Solution:** ARIES-style analysis/redo/undo recovery on restart: analysis
finds where to start, redo replays every logged change (including ones
whose effects already reached disk — redo is idempotent), and undo rolls
back anything logged by a transaction that never committed.

## M8 — Transactions with real atomicity ✅ Done
**Problem:** a group of statements needs all-or-nothing semantics — a
mid-transaction crash or an explicit `ROLLBACK` must undo exactly what that
transaction did, not leave its partial effects in place. Without a WAL to
undo against (M6), there was nothing to roll back to.
**Solution:** `BEGIN`/`COMMIT`/`ROLLBACK` wired to the WAL's undo records
from M7, giving single-transaction atomicity independent of how many other
transactions are (or aren't) running concurrently.

## M9 — Answering point/range lookups without a full scan ✅ Done
**Problem:** a sequential scan is the only access path so far; queries that
touch a small fraction of a table still pay for reading all of it.
**Solution:** a B+tree index and an index-scan operator the planner can
choose over a sequential scan, built against the durable, recoverable
storage layer M6–M8 provide rather than the pre-WAL one — see ADR 0003.

### M9.1 — B+tree node layout and read path ✅ Done
**Problem:** the read path needs a durable, WAL-recoverable tree structure
before insert or index-scan wiring can build on it.
**Solution:** `storage::btree::BTreeIndex` node layout, `create`/`open`/
`get`/`range_scan`, and the encoding contract with
`types::MemcomparableEncode` (`btree.MD`).

### M9.2 — Splitting nodes as they fill ✅ Done
**Problem:** a fixed-size page cannot hold an unbounded number of keys;
`insert` needs a real strategy for what happens when a leaf or internal
node is full.
**Solution:** `BTreeIndex::insert` with leaf and internal node splits,
propagating a new separator key upward.

### M9.3 — Wiring the index into the query path ✅ Done
**Problem:** M9.1 and M9.2 give the tree correct node-level operations,
but nothing yet lets a real `CREATE INDEX` statement build one, or
`SELECT` choose one over a sequential scan.
**Solution:** `leaf_for_start`/`scan_leaf`, an index catalog
(`index_catalog_first_page`) alongside the table catalog, `CREATE INDEX`,
`executor::IndexScanExecutor`, and `planner::optimizer::IndexScanRule`
choosing an index scan over a sequential scan whenever one qualifies.

## M10 — Concurrent transactions without corrupting each other 🚧 In Progress
**Problem:** multiple transactions running at once can interleave their
reads and writes in ways that violate isolation, from lost updates to
dirty reads, on top of the per-transaction atomicity the write-ahead log
already guarantees for each one individually.
**Solution:** in order, a storage layer safe to drive from multiple
threads, a lock manager enforcing two-phase locking, and then MVCC for
snapshot isolation so readers stop blocking writers.

### M10.1 — A storage layer safe to drive from multiple threads ✅ Done
**Problem:** locking and MVCC both assume the storage layer underneath can
be driven from more than one thread at once; before this, nothing
guaranteed that.
**Solution:** `Send + Sync` throughout `storage` — `BufferPool`,
`BlockDevice`, and `SegmentStore` — backed by `Mutex`/`RwLock`/`Condvar`/
atomics rather than any single-threaded assumption, exercised by
`buffer_pool_concurrency.rs` and `dwb_batch_exclusion.rs` driving the
buffer pool from eight threads at once.

### M10.2 — Concurrent execution under two-phase locking 🚧 In Progress
**Problem:** `engine::runtime` buys isolation by refusing concurrency. One
dedicated engine thread executes every statement from every connection
serially, and only one explicit transaction may be open at a time: a
second `BEGIN` is parked in a FIFO queue and eventually fails with
`55P03`. Sessions therefore cannot hold transactions simultaneously, and
throughput is capped at one statement anywhere in the system.
**Solution:** a lock manager enforcing two-phase locking, so isolation
comes from locks rather than from serialization; statements dispatched to
a worker pool instead of the engine thread; the park queue and its
one-transaction-at-a-time limit deleted along with the `55P03` it raised;
and latch crabbing in `storage::btree`, whose descent releases each
parent's guard before fetching the child — safe only while nothing runs
two writers against the tree at once.

### M10.3 — MVCC snapshot isolation 🆕 New
**Problem:** two-phase locking (M10.2) gives correct concurrent execution,
but a reader still blocks behind a writer holding a lock on the same rows,
which a snapshot-isolated database does not require.
**Solution:** multi-version concurrency control so a reader sees a
consistent snapshot without taking row locks, letting readers and writers
stop blocking each other.

## M11 — Surviving a torn page write ✅ Done
**Problem:** even a single page write is not atomic at the hardware level —
a page-sized write can be interrupted mid-sector, leaving a page with some
old bytes and some new ones. That's a different failure mode than anything
above: M5 handles the file being short a whole page, and the WAL (M6–M7)
handles multi-page operations being interrupted between pages, but neither
notices a single page that is itself internally torn, and redo would
otherwise trust such a page as intact.
**Solution:** a CRC-32 checksum per page to detect tearing, and a
double-write buffer — every dirty page's image is written to a separate
file and synced *before* the real write, so a torn real write can be
repaired from that intact copy on the next open instead of merely being
detected. Sequenced ahead of M9 in implementation, before the B+tree
existed: the flush path (`BufferPool::flush_all` and per-page eviction)
was cheaper to change then than once M9's tree splits start writing
several related pages per operation — see M9's entry above, which this
milestone's number-order placement now follows instead of build order.

## M12 — Answering "is this database healthy" from outside the process ✅ Done
**Problem:** everything through M11 makes the engine correct and durable,
but correctness isn't observable from outside the process — an operator
running this in a container has no way to ask "is the buffer pool
thrashing," "did the last shutdown leave torn pages behind," or "has
recovery finished yet" without reading structured logs after the fact.
Docker and Kubernetes also need a machine-checkable answer to a narrower
but load-bearing question: can this container take traffic right now?
ARIES recovery on a large log can take minutes, during which the process
is alive but must not be routed statements yet - a single combined
health check would either have the orchestrator kill it mid-recovery or
route work into a database that can't serve it.
**Solution:** a `metrics`-facade counter/gauge/histogram set covering the
buffer pool, disk, WAL, double-write buffer, checkpoints, transactions,
and recovery, exposed as Prometheus text on its own port; separate
liveness ("the process is up") and readiness ("recovery has completed")
HTTP endpoints, with an explicit `Starting` state between them; a new
headless `server` binary (`crates/server`) built for exactly this,
distinct from the interactive `cli` REPL; and container packaging
(`Dockerfile`, `docker-compose.yml`) with a non-root user, the database
file on a named volume, and `SIGTERM` handled as a graceful checkpoint
-and-close instead of every restart paying for a full crash recovery.

## M13 — Speaking SQL over the network 🚧 In Progress
**Problem:** everything built so far requires an in-process `Database`
handle - `cli`'s REPL and `server`'s metrics/health endpoints both open
the database directly in the same process that uses it. Nothing external
can submit a statement over a network connection, which is what "database
server" ordinarily means and what the existing `server` binary is a
skeleton for.
**Solution:** a PostgreSQL wire protocol frontend via `pgwire`
([sunng87/pgwire](https://github.com/sunng87/pgwire)), so existing
Postgres clients and drivers work against this engine without a bespoke
client library. See `docs/adr/0007-postgres-wire-protocol.md` for why
Postgres's protocol was chosen over Arrow Flight SQL or a bespoke driver.
[datafusion-postgres](https://github.com/datafusion-contrib/datafusion-postgres)
is the reference to study for M13.4's `pg_catalog` support - another
`pgwire`-based engine that had to answer the same catalog-introspection
queries.

### M13.1 — Many connections against one engine ✅ Done
**Problem:** a wire listener serves many connections, each able to submit
a statement at any moment, but `Database` held per-connection state
inside itself and had no entry point reachable from anywhere but its own
thread. Nothing could accept a second connection without either sharing
that state unsafely or opening a second database.
**Solution:** split per-connection session state out of `Database` into
`engine::runtime`'s session table, and give the engine a message-passing
entry point that any number of connection tasks can send statements to
and receive results from, each tagged with its session. How those
statements are then scheduled — serially or concurrently — is the
execution model's business, not this sub-milestone's.

### M13.2 — PostgreSQL wire protocol, simple query 🆕 New
**Problem:** the engine can be reached by message passing from any number
of connections, but nothing yet speaks the bytes a Postgres client
actually sends - startup negotiation, parameter/status exchange, and the
simple query flow all have to work before any real client can connect.
**Solution:** a listener on 5432 using the `pgwire` crate: startup,
`ParameterStatus`, `BackendKeyData`, simple query, type OIDs, and
`SET`/`SHOW`/`RESET` accepted as no-ops (pgjdbc sends `SET
extra_float_digits` during handshake and the connection dies without it).
Target: `psql` works end to end.

### M13.3 — Extended query protocol 🆕 New
**Problem:** simple query (M13.2) inlines literals into full SQL text on
every execution, which is what `psql` does but not what real drivers do -
JDBC and most connection-pooled clients prepare a statement once and
execute it repeatedly with bound parameters, a flow simple query cannot
express.
**Solution:** `Parse`/`Bind`/`Describe`/`Execute`/`Sync`, `$1` placeholders
as a new grammar element, parameter type inference, binary format for
numerics, and `PortalSuspended` for fetch limits. Target: pgjdbc
`PreparedStatement` works. Simple query alone gets a demo, not a driver.

### M13.4 — `pg_catalog` for real SQL clients 🆕 New
**Problem:** ODBC needs no separate driver work - psqlODBC speaks the
same protocol as M13.2/M13.3 - but every real SQL client, ODBC or
otherwise, runs introspection queries against `pg_class`, `pg_namespace`,
`pg_attribute` and `pg_type` before doing anything else, and those queries
need joins this engine does not yet have.
**Solution:** intercept known introspection queries and answer from the
real catalog where the data exists, return empty where it does not, and
never fabricate a result. Track what works in
`docs/CLIENT-COMPATIBILITY.md`. See datafusion-postgres (linked above) as
a reference `pg_catalog` implementation.
**Note:** matching known query text is inherently brittle - it works for
the client versions actually tested and breaks on others that phrase the
same introspection query differently. M24 replaces this interception with
`pg_catalog` tables answered by ordinary queries once joins exist to make
that possible.

## M14 — Changing and removing rows (DELETE, UPDATE, arithmetic) 🆕 New
**Problem:** rows can be inserted and read but never modified or removed.
`DELETE` and `UPDATE` do not exist in the token list, the AST or the
grammar; `TableHeap::delete_tuple` and `update_tuple_in_place` are
reachable only from the catalog's own bookkeeping, and `BTreeIndex::delete`
is `todo!()`.
**Solution:** `DELETE FROM t WHERE ...` and `UPDATE t SET col = expr
WHERE ...`, plus the arithmetic operators (`+`/`-`/`*`/`/`) `UPDATE`'s
`SET` list actually needs: checked arithmetic over `Integer`, `BigInt`
and `Double`, `22003 numeric_value_out_of_range` on overflow, `22012
division_by_zero` on a zero divisor, and `NULL` propagation through every
operator, in both the binder and the executor. The delete/update
executors need each output row's `Rid`, not just its `Tuple`, so
`Executor::next` changes shape to carry both. `BTreeIndex::delete(txn_id,
key, rid)` removes the target entry, located by `key ++ rid` the same way
`insert` places it; an empty leaf is unlinked from the sibling chain (its
page left orphaned but unreachable rather than reclaimed), and a merely
partly empty node is left alone - no merge, no
borrow-from-sibling, the same choice Postgres's `nbtree` makes
(`storage::btree.MD`, `docs/adr/0012-btree-delete-does-not-merge.md`).
Index maintenance on both paths, and in-page compaction in `heap.rs` so
tombstoned space is actually reclaimed, keeping slot indices stable since
a `Rid` is half slot index.
**Constraints for M10.3:** two decisions here are made to avoid a rewrite
once MVCC (M10.3) lands. First, reserve space in the heap tuple header for
version metadata now, even though nothing reads or writes it yet -
retrofitting that space into an on-disk format already in use is a
migration, not a field addition. Second, implement `UPDATE` as a delete of
the old tuple plus an insert of a new one, never as an in-place rewrite of
the tuple's bytes - MVCC needs the old version to remain reachable to a
snapshot that started before the update, which an in-place write
destroys.

## M15 — Column constraints that hold 🆕 New
**Problem:** `Column::nullable` is parsed as a hardcoded `true`, plumbed
through the binder into the catalog, persisted to disk, and never checked
- a schema field that no SQL can set and no code enforces.
`SqlState::NOT_NULL_VIOLATION` is defined and unreachable.
**Solution:** `NOT NULL` in the grammar, enforcement at insert and update
with `23502`, plus `DEFAULT <expr>` and `CHECK (<expr>)`. Add `23514
check_violation` to `SqlState`. No index work required, so this is the
cheapest of the four.
**Note:** `IS NULL`/`IS NOT NULL` (the `Is` token, `Expr::IsNull`,
`BoundExpr::IsNull`, and the executor's evaluation, which returns a real
`Boolean` rather than the `NULL` a bare `= NULL` comparison would) already
landed as B-1, ahead of this milestone rather than as its first step.
Nothing here should reimplement it.

## M16 — Identity and uniqueness (PRIMARY KEY, UNIQUE) 🆕 New
**Problem:** no table can declare a primary key, and the B+tree
deliberately permits duplicates - `get` walks the leaf sibling chain to
collect them. `SqlState::UNIQUE_VIOLATION` is defined and unreachable.
**Solution:** a unique mode on `BTreeIndex::insert` that returns a
violation instead of inserting a duplicate, `UNIQUE` and `PRIMARY KEY`
column and table constraints, an automatically created unique index
backing each, and `PRIMARY KEY` implying `NOT NULL`. Persist the
constraint kind in the index catalog row so it survives a restart. Note
the interaction with M14: uniqueness must be re-checked on `UPDATE`, not
only on `INSERT`.

## M17 — Generated identity (sequences, SERIAL, RETURNING) 🆕 New
**Problem:** every row's primary key has to be supplied by the client.
There are no sequences and no `SERIAL`, so two concurrent inserts cannot
agree on the next id without an external coordinator. M16 gives tables a
primary key but no way to generate one.
**Solution:** sequences as catalog objects with their own durable
counter, `nextval`/`currval`/`setval`, `SERIAL` and `BIGSERIAL` as column
shorthands, and `GENERATED BY DEFAULT AS IDENTITY`. Sequence advances are
non-transactional by design — a rolled-back insert does not return its
id — and that must be stated in the milestone, in the catalog docs and in
an ADR, because it is the one place in the system where a rollback
deliberately does not undo something. ORMs depend on this heavily; expect
every insert from one to end in `RETURNING id`, which means `RETURNING`
belongs here too.

## M18 — Predicates a real query needs (IN, BETWEEN, LIKE) 🆕 New
**Problem:** `WHERE` supports comparison, `AND`/`OR`/`NOT` and arithmetic
and nothing else. `IN`, `BETWEEN` and `LIKE` have no tokens, no AST and
no grammar, so the most common filters an application writes cannot be
expressed. `IN` in particular is what an ORM emits for every
fetch-by-many.
**Solution:** `IN (list)`, `NOT IN`, `BETWEEN ... AND ...`, and `LIKE`
with `%`/`_` and `ESCAPE`. `IN` over a literal list lowers to a
disjunction; leave `IN (subquery)` out of scope, since subqueries do not
exist. Extend `IndexScanRule` so `IN` over an indexed column becomes a
set of range scans rather than a full scan with a filter. (`IS NULL` is
not here — it is B-1, folded into M15.)

## M19 — Shaping the result set (ORDER BY, LIMIT, DISTINCT, aliases) 🆕 New
**Problem:** results come back in physical heap order with no way to
sort, limit, page or deduplicate them, and no way to name a computed
column. `SelectItem` has no alias field. Any application that shows a
list of anything has to fetch the whole table and sort in memory.
**Solution:** `ORDER BY` with `ASC`/`DESC` and `NULLS FIRST`/`LAST`,
`LIMIT`/`OFFSET`, `DISTINCT`, and `AS` aliases in the select list.
`ORDER BY` is where the executor first has to handle a result larger than
memory: implement an external merge sort that spills runs through the
buffer pool rather than assuming everything fits. Teach the optimizer to
skip the sort when an index already provides the requested order.

## M20 — Aggregation 🆕 New
**Problem:** there are no aggregate functions and no `GROUP BY`, so
`SELECT COUNT(*) FROM t` — the single most common query anyone writes
against a new database — is a parse error.
**Solution:** `COUNT`, `SUM`, `AVG`, `MIN`, `MAX` including `COUNT(*)`
and `COUNT(DISTINCT ...)`, `GROUP BY`, `HAVING`, and the NULL semantics
that go with them (aggregates skip NULLs; `COUNT(*)` does not; an empty
group yields `NULL` except `COUNT`, which yields 0). Hash aggregation
with a spill path when the group table exceeds its memory budget, and a
grouping-key check that rejects a select-list column that is neither
grouped nor aggregated with `42803`.

## M21 — Dates, times and exact numerics 🆕 New
**Problem:** `DataType` is `Boolean`, `Integer`, `BigInt`, `Double`,
`Varchar` — no date or time type of any kind, and no exact decimal.
Money cannot be stored without rounding error and a timestamp cannot be
stored at all, which rules out most real schemas.
**Solution:** `DATE`, `TIME`, `TIMESTAMP`, `TIMESTAMPTZ` and `INTERVAL`,
plus `NUMERIC(p, s)`/`DECIMAL` with exact arithmetic. Memcomparable
encodings for each so they can be indexed, `now()`/`current_timestamp`,
date arithmetic against `INTERVAL`, and a text format matching Postgres's
so clients parse it. Do this before M13.2: every column in a
`RowDescription` needs a real Postgres type OID, and mapping a type
system that is still growing means doing that work twice.

## M22 — Authentication and access control 🆕 New
**Problem:** anything that can reach the port is a superuser. There are
no users, no roles and no privileges, and M13.2 opens a socket without
addressing it.
**Solution:** SCRAM-SHA-256 in the startup handler (pgjdbc and psycopg
both negotiate it by default; cleartext and md5 as fallbacks), `CREATE
ROLE`/`ALTER ROLE`/`DROP ROLE`, `GRANT`/`REVOKE` on tables, an owner per
object in the catalog, and a `host`/`user`/`method` access rules file.
Until this lands, the M13.2 listener must bind to `127.0.0.1` by default
and require an explicit opt-in to bind anywhere else — record that as a
constraint in the M13.2 entry, not as a footnote here.

## M23 — Joins and cost-based planning 🚧 In Progress
**Problem:** three things the planner cannot do are really one thing it
cannot do. `FROM` accepts exactly one table, so multi-table questions
have no expression at all. `IndexScanRule` picks an index whenever a
predicate mentions an indexed column, with no idea how selective it is,
so it cannot choose *among* access paths. And with neither joins nor
statistics there is nothing to order a multi-way join by. Each step is
useless without the one before it: a join executor with no cost model
joins in written order, and a cost model with no join to plan is a
number nobody reads.
**Solution:** the three sub-milestones below, in order — the grammar and
executor that make a join possible, then the statistics that make a plan
for it choosable, then the ordering that spends those statistics.

### M23.1 — Answering multi-table queries (nested-loop joins) 🆕 New
**Problem:** multi-table queries cannot be expressed at all today —
`FROM` accepts exactly one table (`crates/sql/src/parser.rs` has no
`JOIN` production and no comma-separated `FROM` list), so any question
spanning two tables has to be answered by the application issuing two
queries and joining the results in memory itself.
**Solution:** in order: `JOIN` syntax and a multi-table `FROM` in the
grammar; a real path into the `LogicalPlan::Join`/
`PhysicalPlan::NestedLoopJoin` node kinds that already exist as
scaffolding but that nothing in `sql`'s grammar can reach today; and
finishing `NestedLoopJoinExecutor`, whose `init` and `next` are both
still `todo!()`. Nested loop only — done when a two-table join returns
correct results. Additional join algorithms and a cost-based optimizer
choosing among them are M23.2, and join ordering is M23.3.
**Note:** the qualified-column representation this sub-milestone needs
(`Expr::Column { table, name }`, `TableRef { name, alias }`, and the
binder's `table_scope` resolution, which makes an aliased table's real
name go out of scope for qualification) already landed ahead of `JOIN`
itself existing. Nothing here should reintroduce it — it is also why the
parent carries 🚧 rather than 🆕.

### M23.2 — Statistics and single-table cost 🆕 New
**Problem:** `IndexScanRule` picks an index whenever a predicate mentions
an indexed column, with no idea how selective it is. An index scan
returning 90% of a table is slower than a sequential scan, and the
planner cannot tell. `planner::optimizer::IndexScanRule` picks an index
scan over a sequential scan whenever one qualifies, greedily and without
comparing cost. What is still needed is choosing *among* several
qualifying access paths (more than one usable index, or an index whose
selectivity doesn't obviously beat a sequential scan) by estimated cost —
a harder problem than the on/off choice M9 already answers, not one M9
left untouched. M23.1's nested-loop join executor picks up here too:
additional join algorithms and a cost-based optimizer choosing among them
and among access paths need the same table and index statistics this
sub-milestone builds.
**Solution:** `ANALYZE`, per-column statistics (row count, distinct
count, null fraction, a histogram or most-common-values list) persisted
in the catalog, selectivity estimation for the predicate forms M18 and
M20 add, and a cost model over sequential and index scans for a single
table. Extend `EXPLAIN` to print estimated rows and cost. Composite /
multi-column index keys belong here too, since the statistics work
assumes them: `crates/types/src/memcomparable.rs` encodes one column's
key today and needs an escaping scheme before it can encode several.
**Note:** `planner::optimizer::IndexScanRule` also skips `BoundExpr::IsNull`
entirely today - `WHERE col IS NULL` on an indexed column is always a
full scan plus filter, never an index scan. `types::memcomparable`
encodes `NULL` as a leading `0x00` tag that sorts before every non-`NULL`
value, so `IS NULL` could become a range scan over `[0x00, 0x01)` the
same way an equality predicate becomes one over `[key, successor(key))`.
Worth doing here, alongside the rest of this sub-milestone's selectivity
work, rather than as a special case bolted onto `IndexScanRule` earlier.

### M23.3 — Join ordering 🆕 New
**Problem:** M23.1 only ever joins tables in the order they're written;
for more than two tables, join order changes the amount of intermediate
data produced by orders of magnitude, and 
M23.2's per-table cost model
says nothing about how to sequence a join yet.
**Solution:** join ordering driven by M23.2's statistics and cost model,
and `EXPLAIN ANALYZE` so estimated rows and cost can be compared against
what a query actually produced — without that, a cost model cannot be
debugged.

## M24 — `pg_catalog` answered by real queries 🆕 New
**Problem:** M13.4's known-query-text interception works for the client
versions it was tested against and breaks on any client that phrases the
same introspection query differently — the moment `pg_class`/
`pg_namespace`/`pg_attribute`/`pg_type` are joined instead of queried
standalone, or filtered or aliased differently, interception has nothing
to match and returns nothing. Real `pg_catalog` compatibility needs those
tables to exist and answer through the same query path every other table
does.
**Solution:** system catalog tables backed by the real `catalog::Catalog`
state and answered through ordinary `SELECT` execution rather than string
matching, including the joins across them real clients issue — which
needs M23.1's nested-loop joins to exist first. Retire M13.4's interception
once these are in place. See datafusion-postgres (linked from M13) as a
reference `pg_catalog` implementation.

## M25 — Referential integrity (foreign keys) 🆕 New
**Problem:** no way to express that one table's column references
another's, so the application has to enforce it and nothing stops an
orphan row.
**Solution:** `FOREIGN KEY ... REFERENCES` in `CREATE TABLE`, validation
on insert and update against the referenced unique index, `ON DELETE` and
`ON UPDATE` actions (`NO ACTION`, `RESTRICT`, `CASCADE`, `SET NULL`), and
constraint metadata in the catalog. Add `23503 foreign_key_violation` to
`SqlState`. State the dependencies explicitly: M14 because the
referential actions are entirely about delete and update behaviour, and
M16 because the referenced column must be backed by a unique index for
the check to be a lookup rather than a scan. Record deferred constraint
checking (`SET CONSTRAINTS DEFERRED`) as explicitly out of scope, since it
needs statement-level rather than row-level checking.

## M26 — Page free list, DROP, TRUNCATE 🆕 New
**Problem:** a table, once created, exists forever. There is no `DROP`,
`ALTER` or `TRUNCATE` in the token list, and `Catalog::drop_table` is a
`todo!()`. A schema mistake means deleting the database file.
`DiskManager::allocate_page` only ever appends, so there is no way to
reclaim a page a dropped table or index frees.
**Solution:** a free-space map the allocator can hand pages back to,
`DROP TABLE`, `DROP INDEX`, and `TRUNCATE` built on it — each has to
reclaim every page the table's heap and indexes owned. `IF EXISTS`
throughout, since every migration tool emits it. This free list is also
where M14's orphaned, unlinked B+tree leaf pages finally get reclaimed.

## M27 — ALTER TABLE 🆕 New
**Problem:** a table's schema is fixed at `CREATE TABLE` time; there is
no way to add, remove or rename a column without dropping and recreating
the table, which loses its data.
**Solution:** `ALTER TABLE` with `ADD COLUMN`, `DROP COLUMN` and
`RENAME`, plus `IF EXISTS`/`IF NOT EXISTS` throughout, since every
migration tool emits them.

## M28 — Logical dump and restore 🆕 New
**Problem:** the only way to back up the database today is to stop the
process and copy three files. There is no way to get data out or back in
as portable SQL.
**Solution:** a logical dump and restore (`pg_dump`-shaped: schema plus
`INSERT`s or a copy stream).

## M29 — Physical backup, WAL archiving, PITR 🆕 New
**Problem:** the only recovery target today is "whatever was in the WAL
when it died" — there is no way to take a backup while the database
keeps running, and no way to recover to a point in time short of that.
Every mechanism needed for something better already exists — segmented
WAL, monotonic LSNs, checkpoints with a recovery bound — and none of it
is exposed.
**Solution:** a physical base backup taken while the database is
running, WAL segment archiving instead of deletion at truncation, and
replay to a target LSN or timestamp. This is the payoff for M5 through
M11 and it is the difference between a durable database and an operable
one. It also gives the crash-injection harness a second oracle: a
restored backup replayed to an LSN must match the live database at that
LSN.
