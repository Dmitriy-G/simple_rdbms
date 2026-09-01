# explain

Renders a `LogicalPlan` or `PhysicalPlan` as the human-readable node tree
`EXPLAIN` prints - one line per plan node, two spaces of indent per
nesting level, with every id resolved to a name through a `Catalog`
rather than shown raw. This is the module `EXPLAIN` exists to justify: a
debugger view of either plan tree is close to unreadable, since every
`Box`ed variant niche-optimizes away the tag a debugger would otherwise
show, and even where it doesn't, a `table_id`/`index_id` means nothing to
a person reading the output.

The two entry points are thin: the actual per-variant formatting lives
next to each enum it renders - `logical_plan::render` beside
`LogicalPlan`, `physical_plan::render` beside `PhysicalPlan` - each an
exhaustive match with no wildcard arm, so adding a plan variant is a
compile error in the same file until its explain line is written, not a
silently-stale `EXPLAIN` output discovered later. What lives here instead
is everything both of those `render` functions share: resolving a
`TableId`/`IndexId` to a name, computing a plan node's output columns,
rendering a `BoundExpr` as SQL-like text, and decoding an index scan's
memcomparable bounds back into a readable condition.

## Key Components

- `explain_logical(plan, catalog, verbose) -> Vec<String>` - renders a
  `LogicalPlan`, one row per node. `verbose` adds an indented continuation
  line under each node with its output columns and types, relevant
  `TableId`/`IndexId`s, and - for an `IndexScan` - the raw memcomparable
  bounds as hex.
- `explain_physical(plan, catalog, verbose) -> Vec<String>` - the same
  rendering for a `PhysicalPlan`.
- `ColumnList` - private type alias for `Vec<(String, Option<DataType>)>`,
  a plan node's output schema as far as `EXPLAIN` can reconstruct it: a
  name and, where one is knowable, a type. `None` only for a bare `NULL`
  literal projected as-is, which genuinely has no type of its own
  (`types::Value::data_type`).
- `table_name(table_id, catalog) -> String`, `index_name(index_id,
  table_id, catalog) -> String`, `column_name(table_id, column_index,
  catalog) -> String` - private. Resolve an id to the name `EXPLAIN`
  prints (`table=users`, not `table_id=3`), falling back to a bracketed
  placeholder like `<table 3>` if the id is somehow missing from the
  catalog - a formatter must never fail a statement over a display
  detail.
- `table_columns(table_id, catalog) -> ColumnList` - private. A table's
  full output schema, in column order, read from its `catalog::Schema`.
- `projected_column(expr, input_cols, position) -> (String, Option<DataType>)` -
  private. The output name and type for one `Projection` expression: a
  `ColumnRef` inherits its source column's name from `input_cols`, and
  every other expression shape (a literal, an operator application, an
  `IsNull` test - always `Boolean`) gets the positional `column<N>` name
  `planner::binder::BoundSelect::column_names` already uses for the same
  case, so `EXPLAIN`'s labels agree with what a client would see as the
  result set's own column names.
- `cols_line(cols) -> String` - private. The non-verbose `cols=[a, b, c]`
  form: names only.
- `verbose_cols_line(cols) -> String` - private. The verbose form, each
  name suffixed `:TYPE` (or `:NULL` when the column's type is unknowable).
- `push_verbose_line(out, depth, parts)` - private. Appends one
  `parts.join(" ")` continuation line indented one level past `depth`,
  unless `parts` is empty, in which case it emits nothing rather than a
  blank line.
- `format_data_type(data_type) -> String` - private. Renders a `DataType`
  the way it was spelled in `CREATE TABLE` (`TEXT`, not
  `Varchar(4294967295)`) - the same convention `cli::format_data_type`
  uses, duplicated rather than shared since `cli` depends on `planner`
  and not the other way around.
- `render_expr(expr, cols) -> String` - private. Renders a `BoundExpr` as
  SQL-like text, resolving every `ColumnRef` through `cols` by ordinal.
  A nested `BinaryOp`/`UnaryOp`/`IsNull` operand is parenthesized
  (`wrapped`); the expression's own top level never wraps itself, which is
  what turns `(age >= 21) AND (name = 'ada')` into exactly that instead of
  `((age >= 21) AND (name = 'ada'))`. `IsNull { expr, negated }` renders as
  `<expr> IS NULL` or `<expr> IS NOT NULL`.
- `wrapped(expr, cols) -> String` - private. `render_expr`, parenthesized
  if `expr` is itself a compound (`BinaryOp`/`UnaryOp`/`IsNull`) node.
- `render_unary(op, expr, cols) -> String`, `op_symbol(op) -> &'static str`,
  `render_value(value) -> String` - private. `render_expr`'s pieces: unary
  operator text, a binary operator's conventional symbol (`=`, `<>`,
  `AND`, ...), and a literal rendered as SQL would spell it (`'ada'` with
  `'` doubled, `TRUE`/`FALSE`, `NULL`).
- `is_successor(start, end) -> bool` - private. Whether `end` is exactly
  `start` with one `0x00` byte appended - `optimizer::successor`'s
  signature, checked byte-for-byte so `render_index_cond` can recognize
  the bound pair `IndexScanRule` produces for `col = value` and collapse
  it back to `=` instead of printing it as a two-sided range.
- `decode_bound(bytes, data_type) -> Option<(Value, bool)>` - private.
  Decodes one bound with `types::decode_memcomparable` and reports
  whether it carries `optimizer::successor`'s trailing `0x00` sentinel,
  which is exactly when `consumed < bytes.len()`. Asserts rather than
  assumes: the only tolerated leftover is a single `0x00` byte
  immediately after the decoded value, so a future encoding change that
  leaves unexpected trailing bytes falls out to `None` (and from there
  to `<undecodable>`) instead of being silently accepted.
- `render_index_cond(index_id, table_id, start, end, catalog) -> String` -
  renders an `IndexScan`'s `Index Cond:` child line: `(no bounds)` if
  both bounds are `None`, `col = value` if `end` is `start`'s successor,
  otherwise the boundary each side's bound actually represents - `start`
  is `col > value` when it carries the sentinel and `col >= value` when
  it doesn't, `end` is `col <= value` when it carries the sentinel and
  `col < value` when it doesn't (`decode_bound`) - joined with `AND` when
  both are present. Decodes each bound against the indexed column's
  `DataType` looked up through `catalog`; any failure along the way
  (unknown index, unknown column, a decode error, an unrecognized
  trailing byte) falls back to the literal string `<undecodable>` rather
  than propagating an error - a broken `EXPLAIN` line must never fail the
  statement that asked for it.
- `render_index_cond_inner(index_id, table_id, start, end, catalog) -> Option<String>` -
  private. `render_index_cond`'s actual logic, written against `Option`'s
  `?` so every failure path falls out to `None` in one place, which the
  public wrapper turns into `<undecodable>`.
- `hex(bytes) -> String` - private. Renders `bytes` as space-separated
  lowercase hex pairs, for `VERBOSE`'s raw memcomparable bounds.

## Usage Example

```rust
use catalog::Catalog;
use planner::{explain_logical, explain_physical};

let lines = explain_physical(&physical_plan, &catalog, false);
for line in lines {
    println!("{line}");
}
```

`engine::Database::execute` calls `explain_physical` (and, under
`VERBOSE`, `explain_logical` first) right after `physical_plan::to_physical`,
turning the returned lines into one `Value::Varchar` row per line under a
single `QUERY PLAN` column - the same shape a real SQL client expects
from `EXPLAIN` (`engine::database.MD`).
