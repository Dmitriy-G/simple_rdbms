# statement_description

The result of `Database::describe`: everything a wire-protocol `Describe`
message needs to answer, learned without ever executing the statement or
touching a transaction. Sits next to `result_set.rs` rather than inside
it - a `ResultSet` is what running a statement produced, while a
`StatementDescription` is what a statement *would* produce if run, plus
the type of every `$n` placeholder it expects - two different questions
with two different shapes, so they stay two different types even though
`runtime.MD`'s `EngineShared::describe` is the only thing that builds
one.

## Key Components

- `StatementDescription` - `param_types: Vec<Option<DataType>>` is one
  entry per `$n` placeholder, indexed by `$n - 1`, exactly as
  `planner::infer_parameter_types` (`planner::parameters.MD`) returns it -
  `None` where no column context in the statement gave the placeholder a
  type. `columns: Vec<(String, Option<DataType>)>` is the result set's
  column names and resolved types, in projection order, taken from a
  `SELECT`'s bound `column_names`/`column_types`
  (`planner::binder::BoundSelect`); empty for any statement that isn't a
  `SELECT`, since only a query has a row shape to describe.

## Usage Example

```rust
use engine::Database;

let description = db.describe("SELECT a, b FROM t WHERE a = $1")?;
assert_eq!(description.param_types.len(), 1);
assert_eq!(description.columns.len(), 2);
```

`runtime::EngineShared::describe` (`runtime.MD`) builds this by inferring
parameter types against the live catalog, substituting a dummy value of
each inferred type so the statement can be bound without real parameter
values, and reading the bound `SELECT`'s column names and types back off
it; `Database::describe` is the public entry point a wire-protocol
handler's `Describe` message answers from.
