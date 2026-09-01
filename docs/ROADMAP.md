# Roadmap

Each milestone is named after the database problem it solves, not the
feature it adds — the feature is just how the problem gets solved this
time. Milestones are numbered by the problem they solve and mostly build
on each other in that order, but implementation order has deliberately
diverged from it twice: M12 shipped ahead of M9 (cheaper to change the
flush path before B+tree splits start writing several related pages per
operation — see M12's entry), and M14 no longer depends on M10 (the
single-threaded engine thread removes the dependency — see M14's entry).
Read each entry's Problem/Solution for what it actually depends on rather
than assuming strict numeric order. Each heading also carries a status:
✅ Done (shipped), 🚧 In Progress (actively being built), or 🆕 New (not
started).

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

## M10 — Concurrent transactions without corrupting each other 🚧 In Progress
**Problem:** multiple transactions running at once can interleave their
reads and writes in ways that violate isolation, from lost updates to
dirty reads, on top of the atomicity M8 already guarantees for each one
individually.
**Solution:** a lock manager enforcing two-phase locking first, then MVCC
for snapshot isolation so readers stop blocking writers.

## M11 — Answering multi-table queries efficiently 🚧 In Progress
**Problem:** multi-table queries cannot be expressed at all today —
`FROM` accepts exactly one table (`crates/sql/src/parser.rs` has no
`JOIN` production and no comma-separated `FROM` list), so any question
spanning two tables has to be answered by the application issuing two
queries and joining the results in memory itself.
**Solution:** in order: `JOIN` syntax and a multi-table `FROM` in the
grammar; a real path into the `LogicalPlan::Join`/
`PhysicalPlan::NestedLoopJoin` node kinds that already exist as
scaffolding but that nothing in `sql`'s grammar can reach today; finishing
`NestedLoopJoinExecutor`, whose `init` and `next` are both still
`todo!()`; and only then additional join algorithms plus a cost-based
optimizer choosing among them and among access paths using table and
index statistics. M9 has since shipped, and a basic form of access-path
choice already exists because of it: `planner::optimizer::IndexScanRule`
picks an index scan over a sequential scan whenever one qualifies,
greedily and without comparing cost. What this milestone's optimizer half
still needs is choosing *among* several qualifying access paths (more
than one usable index, or an index whose selectivity doesn't obviously
beat a sequential scan) by estimated cost, and extending that choice
across a join rather than one table at a time — a harder problem than the
on/off choice M9 already answers, not one M9 left untouched.
**Note:** the qualified-column representation this milestone needs
(`Expr::Column { table, name }`, `TableRef { name, alias }`, and the
binder's `table_scope` resolution, which makes an aliased table's real
name go out of scope for qualification) already landed as B-2, ahead of
`JOIN` itself existing. Nothing here should reintroduce it.

## M12 — Surviving a torn page write ✅ Done
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

## M13 — Answering "is this database healthy" from outside the process ✅ Done
**Problem:** everything through M12 makes the engine correct and durable,
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

## M14 — Speaking SQL over the network 🚧 In Progress
**Problem:** every milestone through M13 still requires an in-process
`Database` handle - `cli`'s REPL and `server`'s metrics/health endpoints
both open the database directly in the same process that uses it. Nothing
external can submit a statement over a network connection, which is what
"database server" ordinarily means and what M13's `server` binary is a
skeleton for.
**Solution:** a PostgreSQL wire protocol frontend via `pgwire`
([sunng87/pgwire](https://github.com/sunng87/pgwire)), so existing
Postgres clients and drivers work against this engine without a bespoke
client library. See `docs/adr/0007-postgres-wire-protocol.md` for why
Postgres's protocol was chosen over Arrow Flight SQL or a bespoke driver.
[datafusion-postgres](https://github.com/datafusion-contrib/datafusion-postgres)
is the reference to study for M14.4's `pg_catalog` support - another
`pgwire`-based engine that had to answer the same catalog-introspection
queries.

**Dependency correction:** the ADR that introduced M14 placed it after
M10 (concurrent transactions), reasoning that a wire protocol implies
multiple connections submitting statements at once, and that M8's
single-threaded, serially-executed atomicity is not the same guarantee as
isolation under real concurrent access. That reasoning assumed the only
way to get real isolation under concurrent access was locking or MVCC -
but M14.1 below runs the engine on a single dedicated thread reached from
every connection by message passing, so statements from all connections
execute serially in arrival order regardless of how many connections are
open, and isolation is genuinely serializable by construction rather than
by locking. This is a **design choice**, not a technical necessity: nothing
in the storage layer forces a single engine thread. Storage is
`Mutex`/`RwLock`/`Condvar`/atomics throughout, and `BufferPool`,
`BlockDevice`, and `SegmentStore` are all `Send + Sync` already -
`buffer_pool_concurrency.rs` and `dwb_batch_exclusion.rs` both drive the
buffer pool from eight threads at once. **M14 does not require M10.**
M10's lock manager and MVCC remain worth building for the concurrency
they add on their own merits - and are exactly what a later milestone
would use to relax the single engine thread into genuine multi-threaded
execution - but are no longer a prerequisite for shipping a network
frontend.

### M14.1 — Many connections against a single-threaded engine ✅ Done
**Problem:** a wire listener needs to serve many concurrent connections,
each of which may submit a statement at any time, but letting each
connection's statements run against a shared `Database` from its own
thread would need real locking or MVCC to stay correct - exactly the
machinery M10 provides, and this milestone deliberately avoids requiring
it yet.
**Solution:** split per-connection session state out of `Database`, run
the engine on one dedicated thread, and reach it from connection tasks by
message passing. Statements execute serially in arrival order, so
isolation stays genuinely serializable rather than merely untested. At
most one explicit transaction is open at a time; a second `BEGIN` waits,
then fails with `55P03`.

### M14.2 — PostgreSQL wire protocol, simple query 🆕 New
**Problem:** M14.1 gives the engine a single-threaded entry point reached
by message passing, but nothing yet speaks the bytes a Postgres client
actually sends - startup negotiation, parameter/status exchange, and the
simple query flow all have to work before any real client can connect.
**Solution:** a listener on 5432 using the `pgwire` crate: startup,
`ParameterStatus`, `BackendKeyData`, simple query, type OIDs, and
`SET`/`SHOW`/`RESET` accepted as no-ops (pgjdbc sends `SET
extra_float_digits` during handshake and the connection dies without it).
Target: `psql` works end to end.

### M14.3 — Extended query protocol 🆕 New
**Problem:** simple query (M14.2) inlines literals into full SQL text on
every execution, which is what `psql` does but not what real drivers do -
JDBC and most connection-pooled clients prepare a statement once and
execute it repeatedly with bound parameters, a flow simple query cannot
express.
**Solution:** `Parse`/`Bind`/`Describe`/`Execute`/`Sync`, `$1` placeholders
as a new grammar element, parameter type inference, binary format for
numerics, and `PortalSuspended` for fetch limits. Target: pgjdbc
`PreparedStatement` works. Simple query alone gets a demo, not a driver.

### M14.4 — `pg_catalog` for real SQL clients 🆕 New
**Problem:** ODBC needs no separate driver work - psqlODBC speaks the
same protocol as M14.2/M14.3 - but every real SQL client, ODBC or
otherwise, runs introspection queries against `pg_class`, `pg_namespace`,
`pg_attribute` and `pg_type` before doing anything else, and those queries
need joins this engine does not yet have.
**Solution:** intercept known introspection queries and answer from the
real catalog where the data exists, return empty where it does not, and
never fabricate a result. Track what works in
`docs/CLIENT-COMPATIBILITY.md`. See datafusion-postgres (linked above) as
a reference `pg_catalog` implementation.

## M15 — Changing and removing rows 🆕 New
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
page reclaimed once M24's free list exists, orphaned but unreachable
until then), and a merely partly empty node is left alone - no merge, no
borrow-from-sibling, the same choice Postgres's `nbtree` makes
(`storage::btree.MD`, `docs/adr/0012-btree-delete-does-not-merge.md`).
Index maintenance on both paths, and the `// TODO(M5): vacuum` compaction
in `heap.rs` so tombstoned space is actually reclaimed, keeping slot
indices stable since a `Rid` is half slot index. Note that this is a hard
prerequisite for M18.

## M16 — Column constraints that hold 🆕 New
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

## M17 — Identity and uniqueness 🆕 New
**Problem:** no table can declare a primary key, and the B+tree
deliberately permits duplicates - `get` walks the leaf sibling chain to
collect them. `SqlState::UNIQUE_VIOLATION` is defined and unreachable.
**Solution:** a unique mode on `BTreeIndex::insert` that returns a
violation instead of inserting a duplicate, `UNIQUE` and `PRIMARY KEY`
column and table constraints, an automatically created unique index
backing each, and `PRIMARY KEY` implying `NOT NULL`. Persist the
constraint kind in the index catalog row so it survives a restart. Note
the interaction with M15: uniqueness must be re-checked on `UPDATE`, not
only on `INSERT`.

## M18 — Referential integrity 🆕 New
**Problem:** no way to express that one table's column references
another's, so the application has to enforce it and nothing stops an
orphan row.
**Solution:** `FOREIGN KEY ... REFERENCES` in `CREATE TABLE`, validation
on insert and update against the referenced unique index, `ON DELETE` and
`ON UPDATE` actions (`NO ACTION`, `RESTRICT`, `CASCADE`, `SET NULL`), and
constraint metadata in the catalog. Add `23503 foreign_key_violation` to
`SqlState`. State the dependencies explicitly: M15 because the
referential actions are entirely about delete and update behaviour, and
M17 because the referenced column must be backed by a unique index for
the check to be a lookup rather than a scan. Record deferred constraint
checking (`SET CONSTRAINTS DEFERRED`) as explicitly out of scope, since it
needs statement-level rather than row-level checking.

## M19 — Predicates a real query needs 🆕 New
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
not here — it is B-1, folded into M16.)

## M20 — Shaping the result set 🆕 New
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

## M21 — Aggregation 🆕 New
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

## M22 — Dates, times and exact numerics 🆕 New
**Problem:** `DataType` is `Boolean`, `Integer`, `BigInt`, `Double`,
`Varchar` — no date or time type of any kind, and no exact decimal.
Money cannot be stored without rounding error and a timestamp cannot be
stored at all, which rules out most real schemas.
**Solution:** `DATE`, `TIME`, `TIMESTAMP`, `TIMESTAMPTZ` and `INTERVAL`,
plus `NUMERIC(p, s)`/`DECIMAL` with exact arithmetic. Memcomparable
encodings for each so they can be indexed, `now()`/`current_timestamp`,
date arithmetic against `INTERVAL`, and a text format matching Postgres's
so clients parse it. Do this before M14.2: every column in a
`RowDescription` needs a real Postgres type OID, and mapping a type
system that is still growing means doing that work twice.

## M23 — Generated identity 🆕 New
**Problem:** every row's primary key has to be supplied by the client.
There are no sequences and no `SERIAL`, so two concurrent inserts cannot
agree on the next id without an external coordinator. M17 gives tables a
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

## M24 — Removing and altering schema objects 🆕 New
**Problem:** a table, once created, exists forever. There is no `DROP`,
`ALTER` or `TRUNCATE` in the token list, and `Catalog::drop_table` is a
`todo!()`. A schema mistake means deleting the database file.
**Solution:** `DROP TABLE`, `DROP INDEX`, `TRUNCATE`, and `ALTER TABLE`
with `ADD COLUMN`, `DROP COLUMN` and `RENAME`. `DROP` has to reclaim
every page the table's heap and indexes owned, which needs the free-space
map the allocator does not have — `DiskManager::allocate_page` only ever
appends. That free list is the real work in this milestone. `IF EXISTS`
and `IF NOT EXISTS` throughout, since every migration tool emits them.

## M25 — Authentication and access control 🆕 New
**Problem:** anything that can reach the port is a superuser. There are
no users, no roles and no privileges, and M14.2 opens a socket without
addressing it.
**Solution:** SCRAM-SHA-256 in the startup handler (pgjdbc and psycopg
both negotiate it by default; cleartext and md5 as fallbacks), `CREATE
ROLE`/`ALTER ROLE`/`DROP ROLE`, `GRANT`/`REVOKE` on tables, an owner per
object in the catalog, and a `host`/`user`/`method` access rules file.
Until this lands, the M14.2 listener must bind to `127.0.0.1` by default
and require an explicit opt-in to bind anywhere else — record that as a
constraint in the M14.2 entry, not as a footnote here.

## M26 — Backup, restore and point-in-time recovery 🆕 New
**Problem:** the only way to back up the database is to stop the process
and copy three files, and the only recovery target is "whatever was in
the WAL when it died". Every mechanism needed for something better
already exists — segmented WAL, monotonic LSNs, checkpoints with a
recovery bound — and none of it is exposed.
**Solution:** a logical dump and restore (`pg_dump`-shaped: schema plus
`INSERT`s or a copy stream), a physical base backup taken while the
database is running, WAL segment archiving instead of deletion at
truncation, and replay to a target LSN or timestamp. This is the payoff
for M5 through M12 and it is the difference between a durable database
and an operable one. It also gives the crash-injection harness a second
oracle: a restored backup replayed to an LSN must match the live database
at that LSN.

## M27 — Statistics and cost-based planning 🆕 New
**Problem:** `IndexScanRule` picks an index whenever a predicate mentions
an indexed column, with no idea how selective it is. An index scan
returning 90% of a table is slower than a sequential scan, and the
planner cannot tell. M11's title says "efficiently", but join ordering
without cardinality estimates is a guess.
**Solution:** `ANALYZE`, per-column statistics (row count, distinct
count, null fraction, a histogram or most-common-values list) persisted
in the catalog, selectivity estimation for the predicate forms M19 and
M21 add, a cost model over sequential and index scans, and join ordering
driven by it. Extend `EXPLAIN` to print estimated rows and cost, and add
`EXPLAIN ANALYZE` so estimates can be compared against reality — without
that, a cost model cannot be debugged.
**Note:** `planner::optimizer::IndexScanRule` also skips `BoundExpr::IsNull`
entirely today - `WHERE col IS NULL` on an indexed column is always a
full scan plus filter, never an index scan. `types::memcomparable`
encodes `NULL` as a leading `0x00` tag that sorts before every non-`NULL`
value, so `IS NULL` could become a range scan over `[0x00, 0x01)` the
same way an equality predicate becomes one over `[key, successor(key))`.
Worth doing here, alongside the rest of this milestone's selectivity
work, rather than as a special case bolted onto `IndexScanRule` earlier.
