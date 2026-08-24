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
