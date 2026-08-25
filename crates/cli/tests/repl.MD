# repl

Unit tests for the pure pieces of the `cli` library - statement buffering,
meta-command parsing, and result-table formatting - driven directly
against `cli`'s public API, now that a library target makes them
reachable from `tests/`. Formerly `statement_from_buffer`'s test lived
inline in `crates/cli/src/main.rs` (`#[cfg(test)] mod tests`) because a
binary-only crate has no library target for an integration test to link
against; moving the REPL's logic into `src/lib.rs` (see
[lib.MD](../src/lib.MD)) made this file possible.

## Key Components

- `only_the_terminating_semicolon_is_stripped` - a statement whose last
  token is itself a string literal ending in `;` (`WHERE s = ';'`) only
  loses the buffer's own terminator, not the semicolon inside the literal.
- `a_statement_without_a_string_literal_still_strips_its_terminator` - the
  ordinary case: a trailing `;` and its surrounding whitespace are
  removed.
- `exit_is_recognized_case_insensitively` - `.EXIT` parses as
  `MetaCommand::Exit`, matching `run_repl`'s case-insensitive check.
- `tables_is_recognized` - `.tables` parses as `MetaCommand::Tables`.
- `schema_captures_the_trimmed_table_name` - `.schema   t  ` parses as
  `MetaCommand::Schema("t")`, with the table name's surrounding whitespace
  trimmed.
- `an_ordinary_statement_is_not_a_meta_command` - a `SELECT` ending in
  `;`, despite starting nothing like a meta-command, parses as `None`.
- `null_values_render_as_the_literal_text_null_in_the_output_table` -
  `format_table` renders a `Value::Null` cell as the literal text `NULL`,
  aligned in its column like any other cell.

## Usage Example

```rust
use cli::{format_table, parse_meta_command, statement_from_buffer, MetaCommand};
use engine::{Tuple, Value};

assert_eq!(parse_meta_command(".tables"), Some(MetaCommand::Tables));

let rows = vec![Tuple::new(vec![Value::Integer(1), Value::Null])];
let table = format_table(&["a".to_string(), "b".to_string()], &rows);
assert!(table.contains("NULL"));
```

Run via `cargo test -p cli --test repl`, alongside
`crates/cli/tests/crash_recovery.rs`'s subprocess-driven durability
tests - this file checks the REPL's parsing and formatting logic in
isolation, without spawning a real `simple_rdbms` process or opening a
`Database` at all.
