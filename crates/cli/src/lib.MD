# lib

The `simple_rdbms` REPL's logic, split out from `main.rs` so it is
reachable from `crates/cli/tests/` - a binary-only crate can't be linked
against by an integration test, so anything worth unit-testing (statement
buffering, meta-command parsing, result-table formatting) has to live in a
library target instead. `main.rs` is left with only argument parsing,
logging setup, and a call into `run_repl`. See
`crates/cli/Cargo.toml`'s `[lib]`/`[[bin]]` split and
`crates/cli/README.md` for how the two targets relate.

## Key Components

- `run_repl(db) -> anyhow::Result<()>` - reads statements from stdin,
  buffering lines until a `;` terminates one, printing that statement's
  result or error before prompting for the next. A leading `.`-command
  (`.tables`, `.schema <name>`, `.exit`) is handled immediately instead,
  without needing a `;`. A query error prints and returns to the prompt;
  it never ends the REPL.
- `statement_from_buffer(buffer) -> &str` - strips the single `;` that
  terminates a buffered statement, along with surrounding whitespace.
  Uses `strip_suffix` rather than `trim_end_matches(';')` so a statement
  whose last token is itself a string literal ending in `;` (e.g. `WHERE
  s = ';'`) only loses the terminator, not the semicolon inside the
  literal.
- `MetaCommand` - a parsed leading-`.` REPL command: `Exit`, `Tables`, or
  `Schema(table_name)`. Exists so meta-command recognition is a pure,
  testable function rather than inline `if`/`else` branches in `run_repl`.
- `parse_meta_command(trimmed) -> Option<MetaCommand>` - recognizes
  `.exit`/`.tables` (case-insensitively) and `.schema <name>` (capturing
  and trimming the name), returning `None` for anything else, including an
  ordinary SQL statement.
- `prompt(stdout, buffer) -> anyhow::Result<()>` - private. Writes the
  REPL's prompt and flushes it, since it is not followed by a newline.
  Mid-statement (a `;` not yet seen), a continuation prompt marks that
  more input is expected.
- `print_tables(db)` - private. `.tables`: lists every table in the
  database, one per line.
- `print_schema(db, name)` - private. `.schema <name>`: prints one line
  per column as `name TYPE [NOT NULL]`.
- `format_data_type(data_type) -> String` - private. Renders a `DataType`
  the way it was spelled in `CREATE TABLE`, e.g. `TEXT` rather than the
  parser's internal `Varchar(4294967295)` for it.
- `format_result(result_set) -> String` - renders a result set: an aligned
  text table for a query's rows, a rows-affected line for a statement with
  no output rows, or `ROLLBACK` for `engine::ResultSet::RolledBack` - a
  `COMMIT` that actually rolled back because its transaction had already
  failed, matching the command tag a real Postgres client would see in the
  same situation. Returns the text to print rather than printing it
  directly, so a test can assert on the rendered string.
- `format_table(columns, rows) -> String` - renders `columns` and `rows`
  as a text table whose columns are padded to the widest cell (header
  included) in that column, one line per row plus a header and separator
  line, each newline-terminated.
- `format_value(value) -> String` - private. Renders one cell's value,
  `NULL` as the literal text `NULL` per spec.

## Usage Example

```rust
use cli::{parse_meta_command, statement_from_buffer, MetaCommand};

assert_eq!(parse_meta_command(".tables"), Some(MetaCommand::Tables));
assert_eq!(statement_from_buffer("SELECT 1;\n"), "SELECT 1");
```

`crates/cli/src/main.rs` calls `run_repl(&mut db)` once, after opening the
`Database` and before closing it - that single call is the entire REPL
session, buffering and executing statements and printing their results
until `.exit` or end of input. `crates/cli/tests/repl.rs` exercises the
pure pieces (`statement_from_buffer`, `parse_meta_command`, `format_table`)
directly, without needing a live `Database` at all.
