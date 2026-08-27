# explain

Exercises `EXPLAIN [VERBOSE] <statement>` through `Database::execute`:
the access path it reports, whether it actually executes anything, and
what `VERBOSE` adds. This is the test that answers the question that
motivated the feature - whether a query used a sequential or index scan -
without a debugger.

## Key Components

- `open(dir) -> Database` - private. Opens a fresh, tempfile-backed
  database, same as every other `tests/*.rs` in this crate.
- `plan_lines(result) -> Vec<String>` - private. Asserts `result` is
  `ResultSet::Rows` with exactly one column named `QUERY PLAN`, and
  unwraps each row's single `Value::Varchar` into a plain `String`,
  panicking on anything else - the shape every `EXPLAIN` result is
  expected to have.
- `rows_of(result) -> Vec<Vec<Value>>` - private. The same row-unwrapping
  helper `tests/integration.rs`/`tests/transactions.rs` use, for asserting
  a table's actual contents are unaffected by an `EXPLAIN`.
- `explain_of_unindexed_select_uses_a_seq_scan_not_an_index_scan` - the
  test that answers the motivating question directly: a table with no
  index explains to a plan containing `Seq Scan` and no `Index Scan`.
- `explain_of_indexed_select_uses_an_index_scan_with_a_decoded_index_cond` -
  with an index on the filtered column, the plan names the index
  (`index=users_age_idx`) and shows a decoded `Index Cond` (`age >= 21`),
  never raw memcomparable bytes.
- `explain_select_from_missing_table_is_undefined_table` - `EXPLAIN
  SELECT` against a table that doesn't exist still fails binding, with
  `SqlState::UNDEFINED_TABLE` (`42P01`) - catching this is half the point
  of binding `EXPLAIN`'s target for real instead of skipping straight to
  formatting.
- `explain_insert_does_not_insert_any_rows` - `EXPLAIN INSERT` reports an
  `Insert` node but the table is empty afterward.
- `explain_insert_inside_open_transaction_leaves_the_transaction_untouched` -
  inside an explicit `BEGIN`, a real `INSERT` followed by an `EXPLAIN
  INSERT` leaves only the real row visible both before and after the
  transaction is later committed - proving `EXPLAIN` never began, joined,
  or otherwise touched the open transaction.
- `explain_verbose_includes_both_plans_plain_explain_only_the_physical_one` -
  plain `EXPLAIN` prints no `Logical plan:`/`Physical plan:` headers at
  all (a single, unlabeled tree); `EXPLAIN VERBOSE` prints both, logical
  first.
- `compound_predicate_shows_index_cond_and_residual_filter` - a `WHERE`
  with one indexable conjunct (`age >= 21`) and one that isn't (`name =
  'ada'`) explains to an `Index Scan` with `Index Cond: age >= 21`
  *and* a `Filter` above it still showing the full original predicate -
  the visible proof that `IndexScanRule` narrows candidates rather than
  fully deciding membership (`planner::optimizer.MD`).

## Usage Example

```rust
let dir = tempfile::tempdir().unwrap();
let mut db = engine::Database::open(common::DbConfig::new(dir.path().join("t.db"))).unwrap();
db.execute("CREATE TABLE t (a INTEGER)").unwrap();
if let engine::ResultSet::Rows { rows, .. } = db.execute("EXPLAIN SELECT * FROM t").unwrap() {
    assert!(!rows.is_empty());
}
```

Run as part of `cargo test --workspace` or `cargo test -p engine`; this
file is `EXPLAIN`'s end-to-end coverage, complementing
`sql/tests/parser_tests.rs`'s grammar-level checks (rejecting `EXPLAIN`
of transaction control and nested `EXPLAIN`, `VERBOSE` not becoming a
reserved word) and `planner`'s unit-level formatter behavior.
