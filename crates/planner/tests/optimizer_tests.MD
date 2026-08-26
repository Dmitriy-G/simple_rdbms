# optimizer_tests

Runs `Optimizer` (configured with `IndexScanRule`) over bound-and-planned
`SELECT`s and checks the resulting `LogicalPlan` shape and `IndexScan`
bounds - complementing `tests/binder_tests.rs`, which stops at binding and
never plans or optimizes.

## Key Components

- `catalog_with_indexed_id_column() -> Catalog` - private. A catalog with
  one table, `t(id INTEGER, name VARCHAR(64))`, and one index,
  `idx_t_id` on `id` (column 0), built via
  `Catalog::from_tables_and_indexes` (no storage backing - see
  `catalog::catalog.MD`).
- `optimized_plan(catalog, source) -> LogicalPlan` - private. Runs the
  full `Lexer::tokenize` + `Parser::parse` + `Binder::bind` +
  `planner::plan` + `Optimizer::optimize` (with `IndexScanRule`) pipeline
  over `source`, panicking with the source and error at whichever stage
  fails (every source string in this file is expected to succeed through
  every stage; the optimizer's rewrite decision is what's under test).
- `filter_input(plan) -> &LogicalPlan` - private. Drills into a `SELECT`
  plan's fixed `Projection(Filter(_))` shape and returns the node under
  the `Filter`, panicking if the plan isn't shaped that way.
- `assert_filter_still_on_top(plan)` - private. Asserts `plan`'s
  `Projection` still wraps a `Filter` (not, say, an `IndexScan` sitting
  directly under the `Projection` with no `Filter` at all) - pins down
  task.MD's "the Filter always stays on top" requirement independent of
  what's under that `Filter`.
- `bounds(catalog, source) -> (Option<Vec<u8>>, Option<Vec<u8>>)` -
  private. `optimized_plan` then `filter_input`, expecting an `IndexScan`
  and returning its `(start, end)`.
- `an_indexed_equality_predicate_is_rewritten_to_an_index_scan` - `WHERE
  id = 5` against the indexed column becomes an `IndexScan` naming the
  right `index_id`/`table_id`, with the `Filter` still on top.
- `a_predicate_on_a_non_indexed_column_stays_a_seq_scan`,
  `a_table_with_no_index_at_all_stays_a_seq_scan` - neither case has a
  usable index for the rule to rewrite around, so both stay a `SeqScan`
  under the `Filter`.
- `equality_produces_an_inclusive_start_and_an_exclusive_end` - `=`
  produces both bounds, with `start < end` (the byte-lexicographic
  ordering `optimizer.rs`'s `successor` relies on).
- `less_than_produces_only_an_exclusive_end`,
  `greater_than_produces_only_an_inclusive_start` - `<`/`>` each
  constrain only the bound on their own side.
- `less_than_or_equal_end_is_strictly_past_the_equality_end`,
  `greater_than_or_equal_start_is_strictly_before_the_greater_than_start` -
  compares `<` against `<=`, and `>` against `>=`, at the same literal, to
  pin down that the inclusive/exclusive variants actually produce
  different (not merely equal-or-looser) bounds via `successor`, rather
  than asserting an exact byte sequence that would be brittle against the
  encoding's internal details.
- `compound_and_on_the_same_column_tightens_both_bounds` - `id > 1 AND id
  < 10` produces exactly the start `id > 1` alone would and the end `id <
  10` alone would - proving the two conjuncts combine rather than one
  overriding the other.
- `a_redundant_and_conjunct_does_not_loosen_an_already_tighter_bound` -
  `id > 5 AND id > 1` keeps the tighter (`> 5`) start, not the looser
  (`> 1`) one - the byte-lexicographic max/min combination in
  `index_scan_bounds`, not just "last conjunct wins".
- `binds_create_index` - `CREATE INDEX idx_t_name ON t (name)` binds to a
  `BoundCreateIndex` naming the right table id and column ordinal (`name`
  is column 1) - the `CREATE INDEX` counterpart to
  `binder_tests.rs::binds_create_table`, kept here rather than there since
  it needs `catalog_with_indexed_id_column`'s fixture table.

## Usage Example

```rust
#[test]
fn example_indexed_equality_becomes_an_index_scan() {
    let catalog = catalog_with_indexed_id_column();
    let plan = optimized_plan(&catalog, "SELECT * FROM t WHERE id = 5");
    assert!(matches!(filter_input(&plan), LogicalPlan::IndexScan { .. }));
}
```

Run as part of `cargo test --workspace`, or just this file with
`cargo test -p planner --test optimizer_tests`.
