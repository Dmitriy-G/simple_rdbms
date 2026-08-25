# main

`simple_rdbms`'s binary entry point: parses command-line arguments,
initializes logging, opens a `Database`, and hands off to the `cli`
library's `run_repl` for the whole REPL session. Everything the REPL
actually does - statement buffering, meta-command parsing, result
formatting - lives in `src/lib.rs` (see [lib.MD](lib.MD)) rather than
here, specifically so it is reachable from `crates/cli/tests/`: a
binary-only crate has no library target for an integration test to link
against, so this crate now ships both (`crates/cli/Cargo.toml`'s
`[lib]`/`[[bin]]` split).

## Key Components

- `Cli` - command-line arguments for the `simple_rdbms` REPL, parsed by
  `clap::Parser`: `db_path`, the path to the database file to open,
  created if it does not already exist (defaults to
  `"simple_rdbms.db"`).
- `main() -> anyhow::Result<()>` - calls `init_logging`, parses `Cli`,
  opens the `Database`, calls `cli::run_repl`, and closes the database.
- `init_logging()` - the one place in the workspace that constructs a
  `tracing` subscriber (libraries emit, only a binary subscribes - see
  CLAUDE.md's logging section): JSON output, level from `RUST_LOG`
  (`EnvFilter::try_from_default_env`), defaulting to `info` if unset.
  Written to **stderr**, not stdout, which is the one deliberate departure
  from CLAUDE.md's "JSON to stdout only" rule: that rule is written for a
  future headless server process, where stdout has no other job, but this
  REPL's stdout is already a tested contract - query results and `OK`/
  `ROLLBACK` acknowledgments (`crates/cli/tests/crash_recovery.rs` reads
  them line by line from a piped child process) - and interleaving JSON
  log lines into it would corrupt that output for any script or human
  reading it. `docker logs`/Kubernetes capture stderr exactly as readily
  as stdout, so nothing about "captured by the container runtime" is lost
  by this choice; only "and never stdout instead" is not honored here,
  specifically because this binary is a REPL and not the future server.

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
process's lifetime, hands it to `cli::run_repl` for the whole session, and
is what `crates/cli/tests/crash_recovery.rs` drives as a real subprocess
to prove acknowledged statements survive a hard kill.
