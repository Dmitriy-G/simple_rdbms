# cli

The `simple_rdbms` REPL: opens a database file, reads SQL statements from
stdin, and prints each one's result or error.

## Architecture

`cli` is the workspace's only binary crate, and sits at the very top of
the layered workspace: it may depend only on `engine` and `common` (see
`docs/adr/0002-crate-splitting.md`), never on any lower-layer crate
directly, since `engine` already re-exports everything a caller needs to
name (`DataType`, `Tuple`, `Value`). It opens exactly one `Database` for
the process's lifetime and is the workspace's only consumer of `engine`
as a long-lived, interactively driven object, rather than one opened and
closed per call. `crates/cli/tests/crash_recovery.rs` drives this binary
as a real subprocess to prove a statement the REPL has acknowledged really
does survive a hard kill.

The REPL loop buffers stdin lines until a `;` terminates a statement (so a
multi-line `CREATE TABLE` works the same as a one-liner), executes that
buffered text against the open `Database`, and prints the result — a
query error prints and returns to the prompt rather than ending the
session. Meta commands (`.tables`, `.schema <name>`, `.exit`) are handled
immediately on a leading `.`, without needing a `;`.

## Key Components

- `main` - `Cli` (parsed `clap` arguments), `main`, `run_repl`, and the REPL's
  supporting functions (`statement_from_buffer`, `prompt`, `print_tables`,
  `print_schema`, `format_data_type`, `print_result`, `print_table`,
  `format_value`). This crate has only one module. See
  [main.MD](src/main.MD).

## Features

`CREATE TABLE`, `INSERT`, `SELECT`, and `BEGIN`/`COMMIT`/`ROLLBACK` all
work through the REPL today, exactly as far as `engine` supports them (see
`engine`'s README for what's not yet implemented below it). The REPL
itself has no additional gaps beyond that: statement buffering, `NULL`
rendering, aligned table output, and the three meta commands all work as
described above.

## Dependencies

Workspace: `common` (for `DbConfig`), `engine` (for `Database`/
`ResultSet`). External: `anyhow`, for `main`/`run_repl`'s error type;
`clap`, deriving `Cli` from command-line arguments; `tracing` and
`tracing-subscriber`, initialized once in `main` for diagnostic logging.
Dev-only: `tempfile`.

## Configuration

The one knob is `db_path` (`Cli::db_path`), the path to the database file
to open — created if it does not already exist, defaulting to
`"simple_rdbms.db"` in the current directory. Every other configuration
knob (page size, buffer pool size, etc.) is left at `DbConfig`'s defaults;
this binary does not currently expose flags for them.

## Testing

`tests/crash_recovery.rs` reproduces the M5 data-loss bug directly: a
statement the REPL has already acknowledged must survive a hard kill of
the process, driving `simple_rdbms` as a real subprocess rather than
calling `engine` in-process. Run just this crate with:

```sh
cargo test -p cli
```
