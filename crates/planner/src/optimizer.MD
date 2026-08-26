# optimizer

Runs a configured sequence of rewrite rules over a `LogicalPlan` before
physical planning, resolving each rule's decisions against a `Catalog` -
e.g. whether an index exists on the column a `Filter` compares.

## Key Components

- `OptimizerRule` - a single rewrite rule applied during logical
  optimization, e.g. choosing an index scan over a sequential scan, or (a
  future rule) pushing a `Filter` below a `Projection`. Each rule is
  self-contained so the optimizer can run a fixed or configurable set of
  them.
- `OptimizerRule::name(&self) -> &'static str` - a short, human-readable
  name for logging and `EXPLAIN` output.
- `OptimizerRule::apply(&self, plan, catalog) -> LogicalPlan` - applies
  this rule to `plan`, returning the rewritten plan. Returns `plan`
  unchanged if the rule does not apply. `catalog` is a read-only reference
  a rule may need to make its decision (e.g. `IndexScanRule` checking
  whether an index exists at all) - rules never mutate the catalog, only
  read it.
- `Optimizer` - runs a configured sequence of `OptimizerRule`s over a
  `LogicalPlan`, then hands the result off for physical planning.
- `Optimizer::new(rules) -> Self` - creates an optimizer that applies
  `rules` in order.
- `Optimizer::optimize(&self, plan, catalog) -> LogicalPlan` - a single
  bottom-up fold, not an iterate-to-fixpoint loop: recurses into every
  child of `plan` first (via the private `recurse_into_children`, which
  matches every `LogicalPlan` variant explicitly - `Filter`/`Projection`/
  `Join` recurse into their input(s), every leaf variant returns itself
  unchanged), then folds `self.rules` over the resulting node in order.
  Bottom-up so a rule inspecting a node's `input` (like `IndexScanRule`
  inspecting a `Filter`'s `SeqScan` child) always sees that child already
  in its final, optimized shape rather than a stale pre-rewrite one - not
  that it matters yet with a single rule, but it is the correctness
  property any rule added later can rely on without re-deriving it.
- `IndexScanRule` - the first, and so far only, rule: rewrites
  `Filter { predicate, input: SeqScan { table_id } }` into
  `Filter { predicate, input: IndexScan { index_id, table_id, start, end } }`
  when `predicate` contains a conjunct usable against an existing index -
  **the `Filter` always stays on top**, re-checking the full, unmodified
  `predicate` against every row the index scan produces. This is what
  makes the rule correct by construction for a multi-conjunct predicate
  (the index only needs to narrow candidates, not fully decide
  membership) and means a bounds-derivation bug would produce missing
  rows rather than wrong ones - dropping a filter that an equality index
  scan alone could already fully satisfy is a `// TODO(M11)` left for the
  cost-based optimizer, not attempted here. Anything that isn't exactly
  this shape (no `Filter`, a `Filter` over something other than a plain
  `SeqScan`, or a `SeqScan` over a table with no usable index) is returned
  unchanged - falling back to a sequential scan is always correct, just
  slower, so this rule never has anything to error on.
- `flatten_and_conjuncts(expr, out)` - private. Recursively flattens a
  predicate's top-level `AND` tree into `out`, left-to-right; a predicate
  with no top-level `AND` is a single conjunct of itself. Stops at `OR`,
  `NOT`, and comparison nodes - it only descends through `AND`, since only
  an `AND`'s conjuncts are all independently required for a row to match
  (an index scan narrowing candidates for one `OR` branch could wrongly
  exclude rows the other branch would have matched).
- `column_and_literal(expr) -> Option<(usize, BinaryOperator, &Value)>` -
  private. Recognizes exactly the shape `ColumnRef { index, .. } <op>
  Literal(value)` with `op` in `Eq | Lt | LtEq | Gt | GtEq`, returning the
  column ordinal, operator, and literal. Nothing else matches - in
  particular `Literal <op> ColumnRef` (operands reversed) does not,
  tracked as the same `// TODO(M11)` as the dropped-filter case above.
- `successor(bytes) -> Vec<u8>` - private. Appends one `0x00` byte,
  producing the tightest possible exclusive upper bound that still admits
  `bytes` itself (used to turn an inclusive bound - `=`'s upper end,
  `<=`'s upper end, `>`'s lower end - into the exclusive form
  `storage::btree::BTreeIndex::range_scan`'s `end`/`leaf_for_start`'s
  `start` expect). Sound for every `types::MemcomparableEncode` encoding
  in this crate, fixed-width or `Varchar`: for two same-type encoded
  values, nothing can encode to a byte string strictly between `bytes` and
  `successor(bytes)`, because either the two encodings diverge within
  their first `bytes.len()` bytes (in which case the comparison is already
  decided there, independent of the appended byte) or they don't (which
  only happens when the other value's encoding literally starts with
  `bytes` followed by more content, which then compares `>=` the appended
  `0x00` byte-for-byte). See `types::memcomparable.MD`'s "Concatenation is
  not safe" for the general caveat this relies on holding per-value, not
  across concatenated values - `successor` is only ever applied to one
  already-complete encoded value here, never to a partial concatenation.
- `IndexScanBounds` - private type alias for `(IndexId, Option<Vec<u8>>,
  Option<Vec<u8>>)`, `index_scan_bounds`'s return shape (named only to
  satisfy `clippy::type_complexity`, not used elsewhere).
- `index_scan_bounds(predicate, table_id, catalog) -> Option<IndexScanBounds>` -
  private. `flatten_and_conjuncts`s `predicate`, then picks the *first*
  conjunct (in source order) that is both a recognized
  `column_and_literal` shape and has an index via
  `catalog.index_for_column` - there is no cost-based choice among
  multiple indexed columns yet (`// TODO(M11)`). Returns `None` (meaning
  "stay a `SeqScan`") if no conjunct qualifies. Once a target column is
  chosen, every conjunct on that *same* column contributes a candidate
  `(start, end)` bound - `Eq` contributes both an inclusive start and an
  exclusive end (via `successor`); `Lt`/`LtEq` contribute only an end;
  `Gt`/`GtEq` contribute only a start - and the final bound takes the
  byte-lexicographic maximum of every candidate start and minimum of
  every candidate end (`Vec<u8>`'s `Ord` is exactly the same
  lexicographic byte comparison `range_scan`/`leaf_for_start` use
  internally), so `col > 1 AND col > 5` and `col > 1 AND col < 10` both
  tighten correctly. A conjunct whose literal fails to
  `encode_memcomparable` (a NaN `Double`) is silently skipped rather than
  erroring - the surrounding `Filter` still re-checks it, so an
  unencodable conjunct just contributes nothing to the index scan's
  bounds instead of failing the statement.

## Usage Example

```rust
use planner::{IndexScanRule, Optimizer};

let optimizer = Optimizer::new(vec![Box::new(IndexScanRule)]);
let optimized = optimizer.optimize(logical_plan, &catalog);
```

`engine::Database::execute_bound`'s `Select` arm constructs an `Optimizer`
with `IndexScanRule` and calls `optimize` between `planner::plan` and
`physical_plan::to_physical`, giving the query a chance at an index scan
before it ever reaches the executor. `INSERT`/DDL statements never go
through `Optimizer` at all - there is nothing in an `Insert`, `CreateTable`,
or `CreateIndex` plan for a rewrite rule to act on yet.
