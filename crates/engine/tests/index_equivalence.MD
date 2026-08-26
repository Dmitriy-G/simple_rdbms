# index_equivalence

The equivalence test - running the same query with and without an index
and asserting identical rows come back, across randomly generated tables
and predicates. This is the test that matters most for M9.3: an
`IndexScanExecutor` bug that returns the wrong rows is a correctness
regression no amount of "does it look like it picked an index" testing
would catch, only comparing its output against the always-correct
sequential scan does.

## Key Components

- `open(dir) -> Database` - opens a `Database` at `dir.join("test.db")`.
  Marked `#[cfg(test)]`, as are every other free function below - see
  `storage/tests/recovery_logging.MD` for why a free function in a
  `tests/` file needs the attribute spelled out explicitly for
  `clippy.toml`'s `allow-expect-in-tests` to apply.
- `build_db(dir, values, with_index) -> Database` - creates `t(a BIGINT)`
  (`BIGINT`, not `INTEGER`, so a negative literal - which binds as
  `BigInt` regardless of column type, see `planner::binder.MD` - never
  needs the cross-type coercion `planner::binder::coerce_literal_to_column`
  only performs for `BoundExpr::Literal`, not for a `UnaryOp::Negate`
  wrapping one; using `BIGINT` sidesteps that pre-existing, out-of-scope
  binder gap entirely rather than working around it inline), optionally
  creates `idx_t_a` on it, then inserts `values` (`NULL` for `None`).
- `query_results(db, predicate) -> Vec<Option<i64>>` - runs `SELECT a FROM
  t WHERE {predicate}` and returns every row's value, sorted (comparing
  as a multiset rather than an exact sequence, since neither a sequential
  nor an index scan's row order is part of this grammar's contract - there
  is no `ORDER BY`).
- `op_strategy() -> impl Strategy<Value = &'static str>` - one of `=`,
  `<`, `<=`, `>`, `>=`.
- `predicate_strategy() -> impl Strategy<Value = String>` - either a
  single `a <op> <literal>` conjunct, or (a third of the time) two
  AND-ed together on the same column - covering both the single-conjunct
  and compound-`AND` bound-derivation paths in
  `planner::optimizer::IndexScanRule`.
- `values_strategy() -> impl Strategy<Value = Vec<Option<i64>>>` - 0 to 40
  rows, each `None` (a `NULL`) or a value in `-20..20` - a narrow range
  deliberately, so generated predicates frequently land inside the data
  and duplicate values are common, rather than mostly missing entirely.
- `index_scan_and_seq_scan_agree_on_every_matching_row` - the `proptest!`
  test: builds two databases from the same generated `values` (one
  indexed, one not), runs the same generated `predicate` against both,
  and asserts identical result sets. Configured with a fixed `rng_seed`
  (`0x5eed_5eed`) rather than proptest's default random seed, per
  CLAUDE.md's testing rules on seeding randomness; 64 cases.

## Usage Example

```rust
proptest! {
    #[test]
    fn example(values in values_strategy(), predicate in predicate_strategy()) {
        let dir_indexed = tempfile::tempdir().unwrap();
        let dir_plain = tempfile::tempdir().unwrap();
        let mut db_indexed = build_db(&dir_indexed, &values, true);
        let mut db_plain = build_db(&dir_plain, &values, false);
        prop_assert_eq!(
            query_results(&mut db_indexed, &predicate),
            query_results(&mut db_plain, &predicate),
        );
    }
}
```

Run as part of `cargo test --workspace`, or just this file with
`cargo test -p engine --test index_equivalence`. A failure is
automatically re-run first on every future invocation via proptest's own
`index_equivalence.proptest-regressions` file (not checked in unless a
real failure is found and fixed, at which point it should be, per
proptest's own convention).
