# ADR 0017: `CREATE INDEX` is a locked writer that publishes only a finished index

Date: 2026-09-09

Status: Accepted

## Context

**This section describes the tree as it stood at the end of M10, when the
milestone review filed P-62**; its citations are historical, and the
Decision below is what the code now does.

Before M10.2 every statement ran on one engine thread, so `CREATE INDEX`
was serialized against every other statement by construction. M10.2 moved
execution onto a worker pool and made isolation the lock manager's job:
`InsertExecutor::init` takes an exclusive table lock, a snapshot reader
takes none, and every lock is held to commit. `CREATE INDEX` was left
out of that scheme entirely.

Its engine arm called `Catalog::create_index`, which allocated an empty
B+tree and installed the new `IndexInfo` into the live in-memory catalog
before returning, and then ran a backfill that scanned the table heap with
`TableHeap::iter` and inserted a key per row. The backfill took no lock,
went through no executor, and applied no visibility filter, and the index
it was filling was already visible to every other session's optimizer.

Four failures followed, all reachable with two sessions and all found by
reading rather than by a bug report:

- A concurrent open transaction's uncommitted row was scanned by the
  backfill and indexed. When that transaction rolled back, undo removed
  the heap row and left an index entry pointing at an empty slot, which a
  later index scan reports as `CorruptTuple` — a hard error on an
  ordinary `SELECT`.
- An `INSERT` that read the table's index list before the new index was
  installed, and wrote its row after the backfill scan had passed that
  page, left a committed row in the heap and never in the index. An index
  scan then silently omitted it.
- The backfill and an `InsertExecutor` could descend the same B+tree with
  two independently cached root page ids, which write-crabbing does not
  protect against.
- Because the empty index was published before the backfill started, a
  *reader* could plan an index scan against a half-built index and get a
  silently short answer — and this one did not need an explicit
  transaction, only a table big enough for the backfill to still be
  running.

The question is what a DDL statement that reads every row of a table and
writes a second structure describing them owes the sessions running
beside it.

## Decision

**`CREATE INDEX` is an ordinary writer of the table it indexes, and an
index becomes visible only once it is complete.** Two rules, both in
`EngineShared::execute_bound`'s `CreateIndex` arm:

- **It takes `LockMode::Exclusive` on the table through the statement's
  own transaction, before it reads anything**, and holds it to commit like
  every other writer, since `LockManager::release_all` runs as the last
  step of commit and abort. `EngineShared::lock_table_exclusive` is the
  one-line helper that clones the `Arc<LockManager>` out of the
  transaction manager under a brief lock, exactly as `EngineShared::run`
  already does.
- **It builds the tree first and registers it afterwards.**
  `EngineShared::build_index` creates the B+tree with
  `BTreeIndex::create`, fills it from the heap keeping the root page id in
  a local variable across splits, and returns the final root;
  `Catalog::create_index_with_root` then writes the catalog row and
  installs the `IndexInfo` in one mutation. `Catalog::create_index` — the
  empty-index constructor the catalog's own tests and
  `executor`'s fixtures use — remains, as a wrapper that creates an empty
  tree and calls the same method.

Together these make the statement atomic from any other session's point of
view: the index is absent, or it is present and complete.

**Why no visibility filter in the backfill.** With the exclusive lock
held, no other transaction can have an uncommitted row in the table, so
every row the scan sees is committed and belongs in the index. Filtering
by the statement's own `read_ts` instead would be actively wrong: a
transaction that committed between this statement's `begin` and its lock
grant is invisible to that snapshot, but its rows are in the heap and must
be in the index, because an index is a physical structure shared by every
transaction and the per-transaction filtering happens in
`IndexScanExecutor` through `VersionStore::is_visible_to`. The lock is
what makes the unfiltered scan correct; the pair is not two overlapping
mechanisms.

**What is deliberately not fixed.** An explicit
`BEGIN; CREATE INDEX ...;` still publishes the index to other sessions
before it commits, because the catalog holds one in-memory state with no
per-transaction visibility (ADR 0014). This is bounded rather than
harmless: the published index is fully built, and every entry in it points
at a committed row, so a reader that uses it gets correct answers; if the
transaction then rolls back, the rollback's `reload_catalog` removes it
again. Making DDL invisible until commit means giving `IndexInfo` a
creating-transaction id and teaching every catalog reader to filter on it,
which is a catalog-wide change and belongs with `DROP TABLE`/`DROP INDEX`
(M26), not here.

The alternative considered was **keeping the publish-then-backfill order
and adding the lock alone**, which is what P-62 recommended. It closes the
three writer failures but not the fourth: an autocommit `CREATE INDEX` on
a large table would still expose an empty index to concurrent readers for
the length of the backfill, and a snapshot reader takes no table lock, so
no lock can close that window. Reordering costs one extra catalog method
and removes the mid-backfill `update_index_root_page` calls as a side
effect, since nothing outside the statement can see the root until it is
final.

## Consequences

`catalog`'s public API gains `Catalog::create_index_with_root`.
`EngineShared::populate_index` is replaced by `EngineShared::build_index`,
which returns a `PageId` instead of mutating catalog state, and the
`CreateIndex` arm no longer calls `Catalog::index_root_page` or
`Catalog::update_index_root_page` at all — those remain the `InsertExecutor`
path's, for a split of an already-published index.

A duplicate index name is now rejected after the backfill rather than
before it, since the name check lives in the same call that publishes.
The error is identical and the transaction aborts either way; what changes
is that a doomed `CREATE INDEX` does its scan first.

`crates/engine/tests/create_index_concurrency.rs` is the regression test:
one case proves `CREATE INDEX` blocks behind an open transaction's
uncommitted `INSERT` and never indexes the row it rolled back, the other
proves an index built beside a concurrent committed `INSERT` returns
exactly what a sequential scan returns.

ADR 0004's "Writers take exclusive locks and hold them to commit" is now
true of every statement that writes, `CREATE INDEX` included; before this
change it was true only of `INSERT`.
