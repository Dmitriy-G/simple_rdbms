# index_scan

End-to-end, deterministic checks of `CREATE INDEX` and index-scan query
correctness through `Database::execute`'s public SQL API, complementing
`tests/index_equivalence.rs`'s randomized index-vs-no-index comparison.

## Key Components

- `open(dir) -> Database` - opens a `Database` at `dir.join("test.db")`.
  Marked `#[cfg(test)]` so `clippy.toml`'s `allow-expect-in-tests` applies
  to its `.expect(...)` call - see `storage/tests/recovery_logging.MD` for
  why a free function in a `tests/` file needs the attribute spelled out
  explicitly.
- `rows_of(result) -> Vec<Vec<Value>>` - unwraps a `ResultSet::Rows` into
  each row's values (dropping the column names `tests/integration.rs`'s
  own `rows_of` keeps, since every test here only ever selects one
  column), panicking on `RowsAffected`/`RolledBack`.
- `index_survives_close_and_reopen_after_a_root_split` - the load-bearing
  regression case for M9.3: inserts 3,000 rows (enough to
  force a B+tree root split at the default 64-frame buffer pool, the same
  scale `crates/storage/tests/btree.rs`'s split tests use) through an
  indexed column, closes and reopens the database, and confirms both a
  point lookup and a full range scan through the index still find
  everything - proving the index's `root_page_id` was persisted correctly
  across the split and the reopen, not just held in memory.
- `null_valued_indexed_column_is_never_returned_by_equality_or_range_predicates` -
  a table with a `NULL` in its indexed column: neither `WHERE a = 1` nor
  `WHERE a > 0` ever returns that row - this is where an index-based
  query engine classically goes wrong, so it gets its own explicit test
  rather than relying only on `tests/index_equivalence.rs`'s randomized
  coverage to catch it.
- `create_index_on_a_nonempty_table_populates_it` - `CREATE INDEX` issued
  *after* rows already exist finds them via a subsequent indexed query,
  exercising `engine::Database::populate_index`.
- `a_rolled_back_create_index_leaves_the_name_reusable` - `BEGIN; CREATE
  INDEX ...; ROLLBACK;` then recreating the same-named index must
  succeed - it would fail with `DuplicateIndex` if the rollback hadn't
  actually removed the first one from the catalog, mirroring
  `crates/engine/tests/transactions.rs`'s
  `rolled_back_create_table_leaves_no_table_and_the_name_is_reusable_with_a_different_schema`.

## Usage Example

```rust
let dir = tempfile::tempdir().expect("create temp dir");
let mut db = open(&dir);
db.execute("CREATE TABLE t (a INTEGER)").expect("create table");
db.execute("CREATE INDEX idx_t_a ON t (a)").expect("create index");
db.execute("INSERT INTO t VALUES (1), (2), (3)").expect("insert");
let rows = rows_of(db.execute("SELECT a FROM t WHERE a = 2").expect("select"));
```

Run as part of `cargo test --workspace`, or just this file with
`cargo test -p engine --test index_scan`.
