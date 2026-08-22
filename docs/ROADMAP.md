# Roadmap

Each milestone is named after the database problem it solves, not the
feature it adds — the feature is just how the problem gets solved this
time. Milestones build on each other in order.

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
which also removes a 4KB write from every page allocation. `Database::sync`
(`flush_all` then `sync`) runs at the end of every mutating statement
(`CREATE TABLE`, `INSERT`), so a statement the caller has already seen
acknowledged is durable before the next one starts, without needing the WAL
this is a stopgap ahead of. This is a stopgap that makes the current engine
honest, not the final design — see ADR 0003 for why the real fix (the WAL)
comes before the B+tree index rather than after it.

## M6 — Making every write atomic and durable
**Problem:** M5 makes a *single* statement's pages durable by the time it's
acknowledged, but says nothing about a statement (or group of statements)
that touches multiple pages: a crash partway through can still leave some
of those pages reflecting the old state and some the new, which is a
different failure mode than the file-truncation M5 closes. There is also no
record of *what* a write changed, which any undo (M8) or redo-after-crash
(M7) needs.
**Solution:** a write-ahead log: every page mutation is described by a log
record appended (and synced) before the page itself is allowed to reach
disk, plus periodic checkpointing so the log doesn't grow without bound.

## M7 — Recovering from a crash
**Problem:** the WAL (M6) only helps if something replays it. On restart
after a crash, the buffer pool is empty and the disk holds whatever mix of
old and new page images the crash left behind; the engine needs to bring
that back to a consistent state before accepting new statements.
**Solution:** ARIES-style analysis/redo/undo recovery on restart: analysis
finds where to start, redo replays every logged change (including ones
whose effects already reached disk — redo is idempotent), and undo rolls
back anything logged by a transaction that never committed.

## M8 — Transactions with real atomicity
**Problem:** a group of statements needs all-or-nothing semantics — a
mid-transaction crash or an explicit `ROLLBACK` must undo exactly what that
transaction did, not leave its partial effects in place. Without a WAL to
undo against (M6), there was nothing to roll back to.
**Solution:** `BEGIN`/`COMMIT`/`ROLLBACK` wired to the WAL's undo records
from M7, giving single-transaction atomicity independent of how many other
transactions are (or aren't) running concurrently.

## M9 — Answering point/range lookups without a full scan
**Problem:** a sequential scan is the only access path so far; queries that
touch a small fraction of a table still pay for reading all of it.
**Solution:** a B+tree index and an index-scan operator the planner can
choose over a sequential scan, built against the durable, recoverable
storage layer M6–M8 provide rather than the pre-WAL one — see ADR 0003.

## M10 — Concurrent transactions without corrupting each other
**Problem:** multiple transactions running at once can interleave their
reads and writes in ways that violate isolation, from lost updates to
dirty reads, on top of the atomicity M8 already guarantees for each one
individually.
**Solution:** a lock manager enforcing two-phase locking first, then MVCC
for snapshot isolation so readers stop blocking writers.

## M11 — Answering multi-table queries efficiently
**Problem:** nested-loop join is the only join strategy, and the planner
always picks it regardless of table sizes or available indexes, which gets
expensive fast.
**Solution:** additional join algorithms and a cost-based optimizer that
chooses among them (and among access paths) using table and index
statistics.

## M12 — Surviving a torn page write
**Problem:** even a single page write is not atomic at the hardware level —
a page-sized write can be interrupted mid-sector, leaving a page with some
old bytes and some new ones. That's a different failure mode than anything
above: M5 handles the file being short a whole page, and the WAL (M6–M7)
handles multi-page operations being interrupted between pages, but neither
notices a single page that is itself internally torn, and redo would
otherwise trust such a page as intact.
**Solution:** a checksum per page to detect tearing, and either a
double-write buffer or full-page WAL images (the first write to a page
after each checkpoint logs the whole page, not just the delta) to
reconstruct it when torn.
