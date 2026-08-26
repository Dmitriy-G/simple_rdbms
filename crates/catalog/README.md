# catalog

The system catalog: table, column, and index metadata. Tracks the schema
of every table and where its heap file begins, and which indexes exist on
which columns, so the planner and executor can resolve names to physical
storage locations without touching disk.

## Architecture

`catalog` sits above `storage` (and, through it, `common`/`types`) and
below `planner`/`executor`, which resolve table, column, and index names
against it (see `docs/adr/0002-crate-splitting.md`). It has no notion of
SQL syntax or query plans — only of tables, columns, indexes, and where
they live.

The catalog persists itself as two ordinary `TableHeap`s: a table catalog
rooted at the database header's `catalog_first_page`, and (M9.3) an index
catalog rooted at `index_catalog_first_page`. `Catalog::create_table`/
`create_index` each append an encoded row to their own heap for every
table/index created, and `Catalog::open` replays both to rebuild the
in-memory indexes at startup. Provisioning either bootstrap heap the very
first time it's needed takes two separate WAL records — the heap's own
`AllocPage` and the header's `Update` pointing the corresponding
`*_first_page` field at it — that must become durable together or not at
all; `tests/bootstrap_atomicity.rs` sweeps every possible crash point
across that sequence to prove a header pointing at a never-allocated heap
can never happen. An index's `root_page_id` is never cached in memory —
see [catalog.MD](src/catalog.MD)'s "No in-memory root-page cache" for why.

## Key Components

- `catalog` - `Catalog`, the in-memory registry of every table's and every
  index's metadata, keyed by name. See [catalog.MD](src/catalog.MD).
- `column` - `Column`, a single column's name, declared type, and
  nullability. See [column.MD](src/column.MD).
- `error` - `CatalogError`, errors raised by the catalog. See
  [error.MD](src/error.MD).
- `index_info` - `IndexInfo`, an index's catalog identity: which index,
  which table and column. See [index_info.MD](src/index_info.MD).
- `persist` - encodes and decodes `TableInfo`/index rows for the catalog's
  own persisted heaps, using `types`'s tuple encoder against fixed row
  schemas. See [persist.MD](src/persist.MD).
- `schema` - `Schema`, an ordered list of columns describing a table's
  tuple shape. See [schema.MD](src/schema.MD).
- `table_info` - `TableInfo`, a table's catalog identity, schema, and heap
  root page. See [table_info.MD](src/table_info.MD).

## Features

`CREATE TABLE`/`CREATE INDEX`, looking up a table or index by name or id,
and listing every table name work today, are crash-safe (see
Architecture), and round-trip across a reopen (`tests/persistence.rs`).
`Catalog::drop_table` does not: its body is `todo!("remove the entry from
tables_by_name, erroring if absent")` — no `DROP TABLE`/`DROP INDEX`
support exists yet anywhere above this crate either, so nothing currently
calls it. Indexes are single-column only — see `docs/ROADMAP.md`'s M11 for
composite keys. There is no `ALTER TABLE` or constraint enforcement
(`CHECK`, `UNIQUE`, foreign keys) beyond nullability bookkeeping — see
`docs/adr/0004-acid-scope.md` for the precise scope of what "consistency"
means in this engine today.

## Dependencies

Workspace: `common`, `types`, `storage` (a table's catalog row is itself
stored through a `TableHeap`/`BufferPool`, and `Catalog::create_table`/
`open` take a `TxnId` for the WAL records that provisioning produces).
External: `thiserror`, for `CatalogError`. Dev-only: `tempfile`.

## Configuration

None — `catalog` has no configuration of its own; it's handed an
already-open `BufferPool` by its caller.

## Testing

`tests/bootstrap_atomicity.rs` is the crash-safety proof described above.
`tests/persistence.rs` checks ordinary (non-crashing) round trips: tables
created against one `BufferPool` reappear, with identical schemas, when the
database file is reopened. `tests/smoke.rs` is the minimum-viable
compile-and-construct check. Run just this crate with:

```sh
cargo test -p catalog
```
