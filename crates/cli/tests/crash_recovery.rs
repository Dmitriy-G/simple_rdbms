//! Reproduces the M5 data-loss bug directly: a statement the REPL has
//! already acknowledged must survive a hard kill of the process, because
//! `Database::execute` syncs every mutating statement to disk before
//! printing its result (see `Database::sync` and its call sites in
//! `Database::execute`). Before that call existed, dirty pages only ever
//! reached disk in `Database::close`/`Drop`, neither of which runs when a
//! process is killed rather than exited normally.
//!
//! M6 changed what `Database::sync` guarantees: it now forces the
//! write-ahead log's `Commit` record to disk (force-at-commit) but
//! deliberately leaves data pages dirty in the buffer pool (no-force), so
//! they may not reach disk until a later eviction or `close`. That is safe
//! *if* a crash can replay the log to redo whatever never made it to the
//! data file - but redo is M7's work, not built yet. Until then, a hard
//! kill genuinely can lose an acknowledged statement's data pages (while
//! still leaving a durable record of it in the WAL), so this test is
//! ignored rather than deleted: it documents the guarantee `Database::sync`
//! is meant to restore once recovery exists, and should be un-ignored as
//! part of implementing it.

use std::error::Error;
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

use common::DbConfig;
use engine::Database;

#[test]
#[ignore = "requires WAL redo (M7); data pages are no-force until then, see module docs"]
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
