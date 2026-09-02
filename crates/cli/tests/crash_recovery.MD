# crash_recovery

Reproduces the M5 data-loss bug directly: a statement the REPL has
already acknowledged must survive a hard kill of the process. Every
mutating statement commits its own transaction before `Database::execute`
returns (see `TransactionManager::commit`'s force-at-commit flush), which
makes the `Commit` record durable but deliberately leaves data pages
dirty in the buffer pool (no-force) - they may not reach disk until a
later eviction or `close`. That is safe because a crash can replay the
log to redo whatever never made it to the data file: `Database::open`
runs `storage::recovery::recover` before the catalog is even loaded (see
M7, `../../../.claude/task.MD`), so a hard kill right after an acknowledgment still leaves
the statement fully recoverable from the WAL alone.

This is the "coarse subprocess SIGKILL sanity check" M7 asks for
alongside the in-process crash-injection harness
(`crates/engine/tests/crash_injection.rs`): it proves the in-process
harness isn't lying about the real syscall path, by actually killing a
real process rather than simulating a write failure.

## Key Components

- `an_acknowledged_statement_survives_a_hard_kill` - spawns
  `simple_rdbms` as a real child process, sends `CREATE TABLE` and
  `INSERT` over its stdin, waits for each `OK` acknowledgment on stdout,
  then hard-kills the process (`Child::kill`'s `SIGKILL` on Unix,
  `TerminateProcess` on Windows - both give the process no chance to run
  its own shutdown code) and reopens the same file, asserting the
  acknowledged insert is present.
- `a_committed_transaction_survives_a_hard_kill_immediately_after_commit`
  - the M8 analogue: runs an explicit `BEGIN`/two `INSERT`s/`COMMIT`
  sequence over the same child-process setup, hard-kills immediately
  after `COMMIT`'s acknowledgment, and asserts both inserts survive -
  proving an explicit transaction's `COMMIT` gives the same force-at-
  commit durability guarantee a bare, autocommit statement does.

## Usage Example

```rust
let mut child = Command::new(env!("CARGO_BIN_EXE_simple_rdbms"))
    .arg(&db_path)
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()?;
// write a statement, read its "OK" acknowledgment, then child.kill()
```

Run as part of `cargo test --workspace`; `CARGO_BIN_EXE_simple_rdbms` ties
this test to the `main.rs` binary directly, so unlike
`crates/engine/tests/crash_injection.rs`'s in-process fault injection,
this exercises the real OS process-kill path end to end.
