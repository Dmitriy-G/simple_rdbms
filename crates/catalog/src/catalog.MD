# catalog

The system catalog: an in-memory registry of every table's and every
index's metadata, keyed by name. Sits between the planner/executor and
`storage`, resolving table, column, and index names to the physical
locations operators read from and write to.

Persisted as two ordinary `TableHeap`s of its own: a table catalog heap
rooted at the database header's `catalog_first_page`, and an index catalog
heap rooted at `index_catalog_first_page` (`storage::disk.MD`), each
following the same pattern. Every `create_table` call appends an encoded
`TableInfo` row to the table catalog heap; every `create_index` call
appends an encoded `(IndexInfo, root_page_id)` row to the index catalog
heap; `open` and `reload` replay both to rebuild the in-memory indexes. A
`Catalog` built with `new` instead of `open` has neither heap yet;
`create_table`/`create_index` each provision their own heap (and record
its root page in the header) the first time they're called with a live
`BufferPool`.

**One live object, and it synchronizes itself.** There is exactly one
`Catalog` for the lifetime of a `Database`, it is never replaced, and no
lock on it is ever held across statement execution
(`docs/adr/0014-catalog-is-a-live-shared-object.md`). The type owns its
own synchronization: a private `RwLock<CatalogState>` guarding the maps,
and a separate `Mutex` — the *mutation lock* — that mutators and reloads
take to exclude each other. Every public method takes `&self`, including
`create_table`, `create_index`, `drop_table` and `reload`; a caller holds
a `&Catalog` and holds no lock at all, because each call takes what it
needs internally and releases it before returning. `engine::EngineShared`
therefore stores an `Arc<Catalog>` with no lock wrapped around it, and the
binder, the optimizer, `explain` and every executor are handed that plain
reference.

Two consequences follow, and both are deliberate. **Lookups return owned
values** — `get_table`, `get_table_by_id`, `index_for_column` and
`indexes_for_table` clone what they find rather than returning a
reference — because a reference into a map behind an internal lock would
have to leak the guard to the caller, which is the statement-length hold
this design exists to remove. These are per-statement or per-operator
calls, never per-row, so the clone is not on a hot path. And **a `SELECT`
blocks nothing**: a reader takes the state read lock for the duration of
one lookup, so a statement that runs for a minute leaves catalog metadata
requests from other sessions answerable throughout
(`engine/tests/dispatch_never_blocks.rs`).

**The mutation lock is what keeps a reload from regressing state.**
`reload` holds it across the whole of its read-from-disk plus install, and
every mutator holds it for that mutator's duration, so a mutation can
never interleave with a reload's read. That is the exact exclusion the
root-page cache below needs: without it, a reload could read an index's
catalog row before a concurrent insert's root split wrote it and install
itself after, reverting the in-memory root page id to a page the real
root's separators no longer cover and silently stranding later inserts
(`engine/tests/catalog_reload_race.rs`). Readers are excluded by neither
lock. **The mutation lock is ordered above every buffer pool latch**: a
mutator holds it across heap writes and a reload holds it across the
catalog heaps' reads, so no code may take it while holding a page guard,
and a reload may block for an unbounded time — which is allowed, because
it only ever runs on a worker thread, never on the dispatch thread.

**Root-page caching.** `IndexInfo` (`index_info.MD`) caches its
`root_page_id` in an `AtomicU32`, plus the two pieces of bookkeeping
needed to update it in O(1): `catalog_rid` (its row's `Rid` in the index
catalog heap, stable for the index's whole lifetime - see below) and
`root_page_id_offset` (the byte offset of `root_page_id` within that row's
own encoding). `index_root_page` is a pure in-memory read;
`update_index_root_page` rewrites just the changed 4 bytes of the durable
row via `storage::heap::TableHeap::update_tuple_in_place` and updates the
cache in the same call, under the mutation lock, so the two can never
observably diverge. Because a lookup hands back a *clone*, the `IndexInfo`
a caller holds is a snapshot: `update_index_root_page` writes through to
the entry stored in the catalog, not to the caller's copy, and a caller
that needs the current value after a split asks `index_root_page` again
rather than re-reading its own clone. `executor::InsertExecutor` keeps its
own `IndexTarget::root_page_id` up to date for exactly this reason.

This cache is only sound because **every abort path reloads the catalog
from disk, not just an explicit `ROLLBACK`.** `storage::btree::BTreeIndex::insert`
mutates `root_page_id` on a split, and if the *rest* of that statement
then fails, WAL undo reverts the durable index-catalog row - if the cache
survived that revert, `index_root_page` would keep returning the
now-wrong, post-split value. Earlier (pre-caching) design deliberately
avoided this by never caching anything; now that a cache exists,
`engine::EngineShared::reload_catalog` calls `Catalog::reload` after
*every* abort - autocommit failure, an explicit `ROLLBACK`, and a `COMMIT`
that turns into a rollback because the transaction was already aborted -
not only the `ROLLBACK` statement, which was the only path that reloaded
before (see `engine::runtime.MD`). Since `reload` rebuilds every
`IndexInfo` from scratch, reading `root_page_id`/`catalog_rid`/
`root_page_id_offset` fresh off the current (post-undo) heap contents,
this reload is what makes caching safe: there is nothing surgical to get
wrong, only "build fresh state and install it." The same applies across a
process restart - `storage::recovery::recover` always runs before
`Catalog::open` (`engine::runtime.rs`'s `EngineShared::open`), so a cache
built by `open` after recovery already reflects every completed redo and
undo.

## Key Components

- `Catalog` - the catalog handle. Holds a `RwLock<CatalogState>` and the
  mutation `Mutex`, and nothing else; every field of the registry lives
  inside `CatalogState`.
- `CatalogState` - private. The registry itself: an in-memory table index
  (`tables_by_name`, `tables_by_id`) plus the next `TableId` to assign and
  the table catalog heap's root page, and an in-memory index registry
  (`indexes_by_name`, `indexes_by_id`, `indexes_by_table` - the last
  mapping a `TableId` to every `IndexId` covering it, for
  `indexes_for_table`) plus the next `IndexId` to assign and the index
  catalog heap's root page. Both root pages are `Option<PageId>`, `None`
  until the corresponding heap is first provisioned. Its private
  `insert_table`/`insert_index` helpers are the one place the several maps
  and the next-id counter are kept consistent with each other, so every
  path that admits an entry - `from_tables`, `reload`, `create_table`,
  `create_index` - goes through them; `table_by_id`/`index_by_id` are the
  double lookups through `tables_by_id`/`indexes_by_id`.
- `Catalog::new() -> Self` - creates an empty catalog with no storage
  backing yet.
- `Catalog::from_tables(tables) -> Self` - builds an in-memory catalog
  directly from already-constructed table entries, with no storage
  backing at all. Intended for tests that need a populated catalog (e.g.
  to exercise the planner's binder) without provisioning a `BufferPool`;
  `create_table` remains the way to register a table that needs real heap
  storage.
- `Catalog::from_tables_and_indexes(tables, indexes) -> Self` -
  `from_tables`, plus registering already-constructed `IndexInfo` entries
  the same in-memory-only way. Intended for tests that need
  `index_for_column`/`indexes_for_table` populated (e.g. to exercise
  `planner::optimizer::IndexScanRule`) without a `BufferPool` or a real
  `storage::btree::BTreeIndex`. `index_root_page` works against a `Catalog`
  built this way (it only reads the in-memory cache each `IndexInfo`
  already carries), but `update_index_root_page` does not - it needs a
  real index catalog heap to write through, which this constructor never
  provisions; `create_index` remains the way to register an index that
  needs one.
- `Catalog::open(buffer_pool, txn_id) -> Result<Self, CatalogError>` -
  creates an empty catalog and `reload`s it, returning the loaded object.
  `txn_id` is the transaction the catalog's own bootstrap heap allocation
  (the first time a database is ever opened) is attributed to; the caller
  is responsible for committing it, so that this bootstrap write is a
  real, committed transaction rather than one crash recovery would undo as
  a loser.
- `Catalog::reload(buffer_pool, txn_id) -> Result<(), CatalogError>` -
  rebuilds the registry from disk and installs it into *this* object,
  under the mutation lock for the whole of both steps. Finds (or
  provisions) the persisted table heap and loads every `TableInfo` row
  from it, then - if an index catalog heap has ever been provisioned (via
  `buffer_pool.index_catalog_first_page()`) - loads every `IndexInfo` the
  same way, caching each row's `root_page_id`/`Rid`/offset fresh from what
  it just read (see "Root-page caching" above). If no index has ever been
  created, the index catalog heap is left unprovisioned rather than
  eagerly created. The fresh state is built into a local `CatalogState`
  and installed with one short write lock at the end, so no reader ever
  observes a half-loaded catalog and no disk I/O happens while the write
  lock is held. Takes `&self`: the object is never replaced, only
  refilled, which is what lets every binder, optimizer and executor keep
  the `&Catalog` it was handed across a reload.
- `Catalog::create_table(buffer_pool, txn_id, name, schema) -> Result<TableInfo, CatalogError>` -
  registers a new table with the given `name` and `schema`, allocating its
  heap storage and a fresh `TableId`, and persists its catalog row.
  Rejects a duplicate `name` (`CatalogError::TableAlreadyExists`). Holds
  the mutation lock across the duplicate check, the heap allocation, the
  catalog-row write and the in-memory install, so two concurrent
  `CREATE TABLE`s cannot both pass the check and claim the same `TableId`.
  Returns the new entry by value.
- `Catalog::get_table(name) -> Result<TableInfo, CatalogError>` - looks up
  a table's metadata by name, returning a clone.
- `Catalog::get_table_by_id(table_id) -> Result<TableInfo, CatalogError>` -
  the same by stable id.
- `Catalog::table_names() -> Vec<String>` - every registered table's name,
  sorted for a deterministic order.
- `Catalog::drop_table(name) -> Result<(), CatalogError>` - removes a
  table's metadata from the catalog. Does not reclaim its heap storage.
  Unimplemented (`todo!`).
- `Catalog::create_index(buffer_pool, txn_id, name, table_id, column_index) -> Result<IndexInfo, CatalogError>` -
  registers a new single-column index: rejects a duplicate `name`
  (`CatalogError::IndexAlreadyExists`), allocates a fresh `IndexId` and an
  empty `storage::btree::BTreeIndex` (via `BTreeIndex::create`), encodes
  and inserts its catalog row (capturing the `Rid` it lands at and the
  byte offset of `root_page_id` within it), and builds the `IndexInfo`
  from all of that in one shot, under the mutation lock throughout.
  Returns it by value. Does not itself populate the index from existing
  rows - `engine::EngineShared::populate_index` does that afterward, the
  same way it would for any row `executor::InsertExecutor` adds later, and
  it runs *outside* the mutation lock so a backfill over a large table
  does not block unrelated catalog work.
- `Catalog::index_for_column(table_id, column_index) -> Option<IndexInfo>` -
  an in-memory-only lookup (no I/O) for whether an index exists on a given
  table's column; what `planner::optimizer::IndexScanRule` calls to decide
  whether a predicate can be rewritten into an index scan.
- `Catalog::indexes_for_table(table_id) -> Vec<IndexInfo>` - every index
  covering `table_id`, in-memory-only; what `executor::InsertExecutor`
  iterates to maintain every index on a table as a row is inserted. A
  `Vec` rather than an iterator because the borrow it would otherwise
  yield points into the map behind the state lock.
- `Catalog::index_root_page(index_id) -> Result<PageId, CatalogError>` -
  the index's *current* root page, read from the in-memory cache in O(1) -
  no `BufferPool` needed at all. `CatalogError::IndexNotFound` if the
  index is somehow missing from the in-memory registry (an
  internal-invariant violation, not expected from any correct caller,
  since any `index_id` a caller has in hand came from that same registry).
  This is the call to use after a split, rather than re-reading a stale
  `IndexInfo` clone.
- `Catalog::update_index_root_page(buffer_pool, txn_id, index_id, new_root) -> Result<(), CatalogError>` -
  rewrites the index's catalog row with `new_root` in place, via
  `storage::heap::TableHeap::update_tuple_in_place` at the index's cached
  `catalog_rid`/`root_page_id_offset` - no scan, and (unlike the
  delete-then-reinsert this replaced) the row's `Rid` never changes, so
  every future call can keep using the same cached one. Updates the stored
  `IndexInfo`'s cached `root_page_id` only *after* the durable write
  succeeds, and holds the mutation lock across both so a reload cannot
  read the row between them. The state lock is *not* held across the write,
  only for the two short lookups either side of it. The caller
  (`executor::InsertExecutor`, or `engine::EngineShared::populate_index`)
  is responsible for calling this whenever `BTreeIndex::insert` changes
  `BTreeIndex::root_page_id()` from what it was before the call.
- `Catalog::ensure_catalog_heap(buffer_pool, txn_id) -> Result<PageId, CatalogError>` -
  private. Returns the table catalog heap's root page, provisioning one
  (and recording it in the database header) if this is the first call
  against a catalog that didn't come from `open`. Assumes its caller holds
  the mutation lock; it takes neither lock for longer than one field read
  or write.
- `Catalog::ensure_index_catalog_heap(buffer_pool, txn_id) -> Result<PageId, CatalogError>` -
  private. The same provisioning as `ensure_catalog_heap`, for the index
  catalog heap and `storage::disk::header::INDEX_CATALOG_FIRST_PAGE_RANGE`
  instead.
- `Catalog::lock_mutation() -> MutexGuard<'_, ()>` - private. Takes the
  mutation lock, recovering from poisoning rather than propagating it: a
  worker that panicked mid-statement must not make every later `CREATE
  TABLE` in the process fail.
- `Catalog::read_state()`/`Catalog::write_state()` - private. The same
  poison-recovering access to the state `RwLock`.

## Usage Example

```rust
use catalog::Catalog;

let catalog = Catalog::open(&buffer_pool, bootstrap_txn)?;
catalog.create_table(&buffer_pool, txn_id, "users", schema)?;
let info = catalog.get_table("users")?;

let index = catalog.create_index(&buffer_pool, txn_id, "idx_users_id", info.table_id, 0)?;
let root = catalog.index_root_page(index.index_id)?;

catalog.reload(&buffer_pool, reload_txn)?;
```

`engine::EngineShared` holds one `Arc<Catalog>` for the lifetime of an
open database, built once via `open` at startup and refilled in place by
`reload` after every abort - not only an explicit `ROLLBACK` - rather than
trying to undo its in-memory effects (including the `root_page_id` cache)
surgically; see "Root-page caching" above for why that reload is
load-bearing now that a cache exists at all. Because the object is never
swapped, a binder or executor holding a `&Catalog` across a reload sees
the refreshed contents rather than a detached stale copy, and nothing in
the engine needs a lock to make that safe.
`tests/bootstrap_atomicity.rs` sweeps every crash point across that very
first `open` to check the catalog's bootstrap heap allocation and the
header pointer to it become durable together or not at all.
