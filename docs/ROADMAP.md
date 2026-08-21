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

## M5 — Answering point/range lookups without a full scan
**Problem:** a sequential scan is the only access path so far; queries that
touch a small fraction of a table still pay for reading all of it.
**Solution:** a B+tree index and an index-scan operator the planner can
choose over a sequential scan.

## M6 — Surviving a crash
**Problem:** a crash mid-write can leave pages on disk in a state that
reflects neither the old nor the new value, or reflects writes from a
transaction that never committed.
**Solution:** a write-ahead log, periodic checkpointing, and ARIES-style
analysis/redo/undo recovery on restart.

## M7 — Concurrent transactions without corrupting each other
**Problem:** multiple transactions running at once can interleave their
reads and writes in ways that violate isolation, from lost updates to
dirty reads.
**Solution:** a lock manager enforcing two-phase locking first, then MVCC
for snapshot isolation so readers stop blocking writers.

## M8 — Answering multi-table queries efficiently
**Problem:** nested-loop join is the only join strategy, and the planner
always picks it regardless of table sizes or available indexes, which gets
expensive fast.
**Solution:** additional join algorithms and a cost-based optimizer that
chooses among them (and among access paths) using table and index
statistics.
