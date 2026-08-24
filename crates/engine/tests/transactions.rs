#![allow(clippy::unwrap_used, clippy::expect_used)]

use common::{DbConfig, SqlState};
use engine::{Database, ResultSet};
use types::Value;

fn open(dir: &tempfile::TempDir) -> Database {
    let config = DbConfig::new(dir.path().join("test.db"));
    Database::open(config).expect("open database")
}

fn rows_of(result: ResultSet) -> Vec<Vec<Value>> {
    match result {
        ResultSet::Rows { rows, .. } => rows.into_iter().map(|t| t.values().to_vec()).collect(),
        ResultSet::RowsAffected(n) => panic!("expected Rows, got RowsAffected({n})"),
        ResultSet::RolledBack => panic!("expected Rows, got RolledBack"),
    }
}

#[test]
fn rollback_of_inserts_leaves_the_table_empty() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let mut db = open(&dir);

    db.execute("CREATE TABLE t (a INTEGER)").expect("create table");
    db.execute("BEGIN").expect("begin");
    db.execute("INSERT INTO t VALUES (1)").expect("insert 1");
    db.execute("INSERT INTO t VALUES (2)").expect("insert 2");
    db.execute("ROLLBACK").expect("rollback");

    assert_eq!(rows_of(db.execute("SELECT * FROM t").expect("select")), Vec::<Vec<Value>>::new());
}

#[test]
fn commit_of_inserts_keeps_them_and_survives_a_reopen() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let path = dir.path().join("test.db");

    {
        let mut db = Database::open(DbConfig::new(&path)).expect("open database");
        db.execute("CREATE TABLE t (a INTEGER)").expect("create table");
        db.execute("BEGIN").expect("begin");
        db.execute("INSERT INTO t VALUES (1)").expect("insert 1");
        db.execute("INSERT INTO t VALUES (2)").expect("insert 2");
        db.execute("COMMIT").expect("commit");
        db.close().expect("close database");
    }

    let mut db = Database::open(DbConfig::new(&path)).expect("reopen database");
    assert_eq!(
        rows_of(db.execute("SELECT * FROM t").expect("select")),
        vec![vec![Value::Integer(1)], vec![Value::Integer(2)]]
    );
}

#[test]
fn rolled_back_create_table_leaves_no_table_and_the_name_is_reusable_with_a_different_schema() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let mut db = open(&dir);

    db.execute("BEGIN").expect("begin");
    db.execute("CREATE TABLE t (a INTEGER)").expect("create table");
    db.execute("ROLLBACK").expect("rollback");

    assert!(db.table_names().is_empty(), "the rolled-back table must not exist");

    db.execute("CREATE TABLE t (a INTEGER, b TEXT)").expect("recreate with a different schema");
    db.execute("INSERT INTO t VALUES (1, 'x')").expect("insert into the new table");
    assert_eq!(
        rows_of(db.execute("SELECT * FROM t").expect("select")),
        vec![vec![Value::Integer(1), Value::Varchar("x".to_string())]]
    );
}

#[test]
fn a_select_inside_an_explicit_transaction_does_not_end_it() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let mut db = open(&dir);

    db.execute("CREATE TABLE t (a INTEGER)").expect("create table");
    db.execute("BEGIN").expect("begin");
    db.execute("INSERT INTO t VALUES (1)").expect("insert");
    rows_of(db.execute("SELECT * FROM t").expect("select mid-transaction"));
    assert!(db.execute("BEGIN").is_err(), "the transaction begun above must still be open");
    db.execute("ROLLBACK").expect("rollback");
}

#[test]
fn nested_begin_is_an_error() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let mut db = open(&dir);

    db.execute("BEGIN").expect("begin");
    assert!(db.execute("BEGIN").is_err(), "nesting BEGIN must be rejected");
    db.execute("ROLLBACK").expect("rollback so the transaction doesn't leak into other tests");
}

#[test]
fn commit_with_no_active_transaction_is_an_error() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let mut db = open(&dir);
    assert!(db.execute("COMMIT").is_err());
}

#[test]
fn rollback_with_no_active_transaction_is_an_error() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let mut db = open(&dir);
    assert!(db.execute("ROLLBACK").is_err());
}

#[test]
fn a_failed_statement_inside_begin_aborts_the_transaction_until_rollback() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let mut db = open(&dir);

    db.execute("CREATE TABLE t (a INTEGER)").expect("create table");
    db.execute("BEGIN").expect("begin");
    db.execute("INSERT INTO t VALUES (1)").expect("insert 1");

    let err = db.execute("SELECT * FROM missing").expect_err("selecting a missing table must fail");
    assert_eq!(err.sql_state(), SqlState::UNDEFINED_TABLE);

    let err = db
        .execute("INSERT INTO t VALUES (2)")
        .expect_err("a well-formed statement after the abort must still be rejected");
    assert_eq!(err.sql_state(), SqlState::IN_FAILED_SQL_TRANSACTION);

    db.execute("ROLLBACK").expect("rollback clears the aborted transaction");

    db.execute("BEGIN").expect("a new transaction can begin after rollback");
    rows_of(db.execute("SELECT * FROM t").expect("statements work again after rollback"));
    db.execute("ROLLBACK").expect("rollback so the transaction doesn't leak into other tests");
}

#[test]
fn commit_of_an_aborted_transaction_rolls_back_and_reports_it() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let mut db = open(&dir);

    db.execute("CREATE TABLE t (a INTEGER)").expect("create table");
    db.execute("BEGIN").expect("begin");
    db.execute("INSERT INTO t VALUES (1)").expect("insert 1");
    db.execute("SELECT * FROM missing").expect_err("selecting a missing table must fail");

    let result = db.execute("COMMIT").expect("COMMIT on an aborted transaction is not an error");
    assert_eq!(result, ResultSet::RolledBack);

    assert_eq!(rows_of(db.execute("SELECT * FROM t").expect("select")), Vec::<Vec<Value>>::new());
}

#[test]
fn a_bare_insert_outside_any_transaction_is_durable_once_execute_returns() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let path = dir.path().join("test.db");

    {
        let mut db = Database::open(DbConfig::new(&path)).expect("open database");
        db.execute("CREATE TABLE t (a INTEGER)").expect("create table");
        db.execute("INSERT INTO t VALUES (1)").expect("autocommit insert");
    }

    let mut db = Database::open(DbConfig::new(&path)).expect("reopen database");
    assert_eq!(
        rows_of(db.execute("SELECT * FROM t").expect("select")),
        vec![vec![Value::Integer(1)]]
    );
}
