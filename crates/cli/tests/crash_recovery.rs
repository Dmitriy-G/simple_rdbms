//! Reproduces the M5 data-loss bug directly: a statement the REPL has
//! already acknowledged must survive a hard kill of the process. Every
//! mutating statement commits its own transaction before `Database::execute`
//! returns (see `TransactionManager::commit`'s force-at-commit flush), which
//! makes the `Commit` record durable but deliberately leaves data pages
//! dirty in the buffer pool (no-force) - they may not reach disk until a
//! later eviction or `close`. That is safe because a crash can replay the
//! log to redo whatever never made it to the data file: `Database::open`
//! runs `storage::recovery::recover` before the catalog is even loaded (see
//! M7, `task.MD`), so a hard kill right after an acknowledgment still
//! leaves the statement fully recoverable from the WAL alone.
//!
//! This is the "coarse subprocess SIGKILL sanity check" M7 asks for
//! alongside the in-process crash-injection harness
//! (`crates/engine/tests/crash_injection.rs`): it proves the in-process
//! harness isn't lying about the real syscall path, by actually killing a
//! real process rather than simulating a write failure.

use std::error::Error;
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

use common::DbConfig;
use engine::Database;

#[test]
fn an_acknowledged_statement_survives_a_hard_kill() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let db_path = dir.path().join("test.db");

    let mut child = Command::new(env!("CARGO_BIN_EXE_simple_rdbms"))
        .arg(&db_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let mut stdin = child.stdin.take().ok_or("child stdin was not piped")?;
    let stdout = child.stdout.take().ok_or("child stdout was not piped")?;
    let mut lines = BufReader::new(stdout).lines();

    writeln!(stdin, "CREATE TABLE t (a INTEGER);")?;
    let ack = lines.next().ok_or("child exited before acknowledging CREATE TABLE")??;
    assert!(ack.contains("OK"), "expected an OK acknowledgment for CREATE TABLE, got: {ack}");

    writeln!(stdin, "INSERT INTO t VALUES (1);")?;
    let ack = lines.next().ok_or("child exited before acknowledging INSERT")??;
    assert!(ack.contains("OK"), "expected an OK acknowledgment for INSERT, got: {ack}");

    // A hard kill, not a graceful exit: no `close`, no `Drop`. On Unix this
    // is `Child::kill`'s `SIGKILL`; on Windows it's `TerminateProcess` - both
    // give the process no chance to run its own shutdown code.
    child.kill()?;
    child.wait()?;
    drop(stdin);

    let mut db = Database::open(DbConfig::new(db_path))?;
    assert_eq!(db.table_names(), vec!["t".to_string()]);

    let result = db.execute("SELECT a FROM t")?;
    let engine::ResultSet::Rows { rows, .. } = result else {
        return Err("expected a Rows result set from SELECT".into());
    };
    assert_eq!(rows.len(), 1, "the acknowledged INSERT must have survived the kill");
    assert_eq!(rows[0].values(), &[engine::Value::Integer(1)]);

    Ok(())
}

/// The M8 analogue of the test above: an explicit transaction's `COMMIT`
/// gives the same force-at-commit durability guarantee a bare, autocommit
/// statement does - a hard kill immediately after `COMMIT` is acknowledged
/// must not lose any of the transaction's writes.
#[test]
fn a_committed_transaction_survives_a_hard_kill_immediately_after_commit()
-> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let db_path = dir.path().join("test.db");

    let mut child = Command::new(env!("CARGO_BIN_EXE_simple_rdbms"))
        .arg(&db_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let mut stdin = child.stdin.take().ok_or("child stdin was not piped")?;
    let stdout = child.stdout.take().ok_or("child stdout was not piped")?;
    let mut lines = BufReader::new(stdout).lines();

    for statement in [
        "CREATE TABLE t (a INTEGER);",
        "BEGIN;",
        "INSERT INTO t VALUES (1);",
        "INSERT INTO t VALUES (2);",
        "COMMIT;",
    ] {
        writeln!(stdin, "{statement}")?;
        let ack =
            lines.next().ok_or(format!("child exited before acknowledging {statement}"))??;
        assert!(ack.contains("OK"), "expected an OK acknowledgment for {statement}, got: {ack}");
    }

    // A hard kill right after COMMIT returns - no `close`, no `Drop`.
    child.kill()?;
    child.wait()?;
    drop(stdin);

    let mut db = Database::open(DbConfig::new(db_path))?;
    let result = db.execute("SELECT a FROM t")?;
    let engine::ResultSet::Rows { rows, .. } = result else {
        return Err("expected a Rows result set from SELECT".into());
    };
    assert_eq!(rows.len(), 2, "both inserts from the committed transaction must survive the kill");
    assert_eq!(rows[0].values(), &[engine::Value::Integer(1)]);
    assert_eq!(rows[1].values(), &[engine::Value::Integer(2)]);

    Ok(())
}
