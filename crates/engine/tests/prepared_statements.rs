use common::{DbConfig, Error, SqlState};
use engine::{Database, ResultSet};
use types::{DataType, Value};

#[cfg(test)]
fn open(dir: &tempfile::TempDir) -> Database {
    let config = DbConfig::new(dir.path().join("test.db"));
    Database::open(config).expect("open database")
}

fn rows_and_columns_of(result: ResultSet) -> (Vec<String>, Vec<Vec<Value>>) {
    match result {
        ResultSet::Rows { columns, rows, .. } => {
            (columns, rows.into_iter().map(|t| t.values().to_vec()).collect())
        }
        ResultSet::RowsAffected(n) => panic!("expected Rows, got RowsAffected({n})"),
        ResultSet::RolledBack => panic!("expected Rows, got RolledBack"),
    }
}

#[test]
fn describe_reports_parameter_and_column_types() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let mut db = open(&dir);
    db.execute("CREATE TABLE t (a INTEGER, b TEXT)").expect("create table");

    let description = db.describe("SELECT a, b FROM t WHERE a = $1").expect("describe");
    assert_eq!(description.param_types, vec![Some(DataType::Integer)]);
    assert_eq!(
        description.columns,
        vec![
            ("a".to_string(), Some(DataType::Integer)),
            ("b".to_string(), Some(DataType::Varchar(u32::MAX))),
        ]
    );
}

#[test]
fn describe_leaves_no_transaction_or_lock_behind() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let mut db = open(&dir);
    db.execute("CREATE TABLE t (a INTEGER, b TEXT)").expect("create table");

    db.describe("SELECT a, b FROM t WHERE a = $1").expect("describe");

    assert!(
        db.current_txn_id().expect("current txn id").is_none(),
        "describe must not leave an open transaction on the session"
    );

    db.execute("CREATE INDEX idx_t_a ON t (a)")
        .expect("a lock describe might have held would block this");
}

#[test]
fn execute_with_params_inserts_and_selects_with_bound_values_in_autocommit() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let mut db = open(&dir);
    db.execute("CREATE TABLE t (a INTEGER, b TEXT)").expect("create table");

    db.execute_with_params(
        "INSERT INTO t VALUES ($1, $2)",
        &[Value::Integer(1), Value::Varchar("alice".to_string())],
    )
    .expect("insert with params");

    let (columns, rows) = rows_and_columns_of(
        db.execute_with_params("SELECT a, b FROM t WHERE a = $1", &[Value::Integer(1)])
            .expect("select with params"),
    );
    assert_eq!(columns, vec!["a", "b"]);
    assert_eq!(rows, vec![vec![Value::Integer(1), Value::Varchar("alice".to_string())]]);
}

#[test]
fn execute_with_params_works_inside_an_explicit_transaction() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let mut db = open(&dir);
    db.execute("CREATE TABLE t (a INTEGER)").expect("create table");

    db.execute("BEGIN").expect("begin");
    db.execute_with_params("INSERT INTO t VALUES ($1)", &[Value::Integer(7)])
        .expect("insert with params inside an explicit transaction");
    db.execute("COMMIT").expect("commit");

    let (_, rows) = rows_and_columns_of(db.execute("SELECT a FROM t").expect("select"));
    assert_eq!(rows, vec![vec![Value::Integer(7)]]);
}

#[test]
fn wrong_parameter_count_is_a_protocol_violation() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let mut db = open(&dir);
    db.execute("CREATE TABLE t (a INTEGER)").expect("create table");

    let err = db
        .execute_with_params("INSERT INTO t VALUES ($1)", &[Value::Integer(1), Value::Integer(2)])
        .expect_err("expected a parameter count mismatch");
    assert_eq!(err.sql_state(), SqlState::PROTOCOL_VIOLATION);
    assert!(matches!(err, Error::ParameterCountMismatch { expected: 1, found: 2 }));
}

#[test]
fn plain_execute_on_parameterized_sql_is_undefined_parameter() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let mut db = open(&dir);
    db.execute("CREATE TABLE t (a INTEGER)").expect("create table");

    let err =
        db.execute("SELECT * FROM t WHERE a = $1").expect_err("expected an undefined parameter");
    assert_eq!(err.sql_state(), SqlState::UNDEFINED_PARAMETER);
    assert!(matches!(err, Error::UndefinedParameter { index: 1 }));
}
