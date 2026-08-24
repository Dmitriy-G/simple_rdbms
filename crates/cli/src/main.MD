# main

`simple_rdbms`: the REPL front end. Opens a database file and reads SQL
statements from stdin, buffering lines until a `;` terminates a
statement, executing each against the open `Database` and printing its
result or error.

## Key Components

- `Cli` - command-line arguments for the `simple_rdbms` REPL, parsed by
  `clap::Parser`: `db_path`, the path to the database file to open,
  created if it does not already exist (defaults to
  `"simple_rdbms.db"`).
- `main() -> anyhow::Result<()>` - initializes tracing, parses `Cli`,
  opens the `Database`, runs the REPL, and closes the database.
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
- `prompt(stdout, buffer) -> anyhow::Result<()>` - writes the REPL's
  prompt and flushes it, since it is not followed by a newline.
  Mid-statement (a `;` not yet seen), a continuation prompt marks that
  more input is expected.
- `print_tables(db)` - `.tables`: lists every table in the database, one
  per line.
- `print_schema(db, name)` - `.schema <name>`: prints one line per column
  as `name TYPE [NOT NULL]`.
- `format_data_type(data_type) -> String` - renders a `DataType` the way
  it was spelled in `CREATE TABLE`, e.g. `TEXT` rather than the parser's
  internal `Varchar(4294967295)` for it.
- `print_result(result_set)` - prints a result set: an aligned text table
  for a query's rows, `NULL` for SQL `NULL`, or a rows-affected line for
  a statement with no output rows.
- `print_table(columns, rows)` - prints `columns` and `rows` as a text
  table whose columns are padded to the widest cell (header included) in
  that column.
- `format_value(value) -> String` - renders one cell's value, `NULL` as
  the literal text `NULL` per spec.

## Usage Example

```sh
cargo run -p cli -- my.db
simple_rdbms> CREATE TABLE t (a INTEGER);
OK
simple_rdbms> INSERT INTO t VALUES (1);
OK (1 row)
simple_rdbms> SELECT * FROM t;
a
---
1
simple_rdbms> .exit
```

This binary is the workspace's only consumer of `engine::Database` as a
long-lived, interactively driven object: it opens one `Database` for the
process's lifetime, feeds it one statement per `;`-terminated buffer, and
is what `crates/cli/tests/crash_recovery.rs` drives as a real subprocess
to prove acknowledged statements survive a hard kill.
