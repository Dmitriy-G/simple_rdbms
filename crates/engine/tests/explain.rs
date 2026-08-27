use common::{DbConfig, SqlState};
use engine::{Database, ResultSet};
use types::Value;

#[cfg(test)]
fn open(dir: &tempfile::TempDir) -> Database {
    let config = DbConfig::new(dir.path().join("test.db"));
    Database::open(config).expect("open database")
}

fn plan_lines(result: ResultSet) -> Vec<String> {
    match result {
        ResultSet::Rows { columns, rows } => {
            assert_eq!(columns, vec!["QUERY PLAN".to_string()]);
            rows.into_iter()
                .map(|tuple| match &tuple.values()[0] {
                    Value::Varchar(line) => line.clone(),
                    other => panic!("expected a Varchar QUERY PLAN row, got {other:?}"),
                })
                .collect()
        }
        ResultSet::RowsAffected(n) => panic!("expected Rows, got RowsAffected({n})"),
        ResultSet::RolledBack => panic!("expected Rows, got RolledBack"),
    }
}

fn rows_of(result: ResultSet) -> Vec<Vec<Value>> {
    match result {
        ResultSet::Rows { rows, .. } => rows.into_iter().map(|t| t.values().to_vec()).collect(),
        ResultSet::RowsAffected(n) => panic!("expected Rows, got RowsAffected({n})"),
        ResultSet::RolledBack => panic!("expected Rows, got RolledBack"),
    }
}

#[test]
fn explain_of_unindexed_select_uses_a_seq_scan_not_an_index_scan() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let mut db = open(&dir);
    db.execute("CREATE TABLE t (a INTEGER)").expect("create table");

    let lines = plan_lines(db.execute("EXPLAIN SELECT * FROM t").expect("explain"));
    let plan = lines.join("\n");

    assert!(plan.contains("Seq Scan"), "expected a Seq Scan, got:\n{plan}");
    assert!(!plan.contains("Index Scan"), "expected no Index Scan, got:\n{plan}");
}

#[test]
fn explain_of_indexed_select_uses_an_index_scan_with_a_decoded_index_cond() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let mut db = open(&dir);
    db.execute("CREATE TABLE users (id INTEGER, age INTEGER)").expect("create table");
    db.execute("CREATE INDEX users_age_idx ON users (age)").expect("create index");

    let lines =
        plan_lines(db.execute("EXPLAIN SELECT * FROM users WHERE age >= 21").expect("explain"));
    let plan = lines.join("\n");

    assert!(plan.contains("Index Scan"), "expected an Index Scan, got:\n{plan}");
    assert!(plan.contains("index=users_age_idx"), "expected the index name, got:\n{plan}");
    let index_cond_line = lines
        .iter()
        .find(|line| line.contains("Index Cond:"))
        .unwrap_or_else(|| panic!("expected an Index Cond line, got:\n{plan}"));
    assert_eq!(index_cond_line.trim(), "Index Cond: age >= 21");
}

#[test]
fn explain_select_from_missing_table_is_undefined_table() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let mut db = open(&dir);

    let err = db.execute("EXPLAIN SELECT * FROM ghosts").expect_err("expected an error");
    assert_eq!(err.sql_state(), SqlState::UNDEFINED_TABLE);
}

#[test]
fn explain_insert_does_not_insert_any_rows() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let mut db = open(&dir);
    db.execute("CREATE TABLE t (a INTEGER)").expect("create table");

    let lines = plan_lines(db.execute("EXPLAIN INSERT INTO t VALUES (1)").expect("explain"));
    assert!(lines.iter().any(|line| line.contains("Insert")), "expected an Insert node");

    assert_eq!(rows_of(db.execute("SELECT * FROM t").expect("select")), Vec::<Vec<Value>>::new());
}

#[test]
fn explain_insert_inside_open_transaction_leaves_the_transaction_untouched() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let mut db = open(&dir);
    db.execute("CREATE TABLE t (a INTEGER)").expect("create table");

    db.execute("BEGIN").expect("begin");
    db.execute("INSERT INTO t VALUES (1)").expect("insert the real row");
    db.execute("EXPLAIN INSERT INTO t VALUES (2)").expect("explain insert");

    assert_eq!(
        rows_of(db.execute("SELECT * FROM t").expect("select still inside the open transaction")),
        vec![vec![Value::Integer(1)]]
    );

    db.execute("COMMIT").expect("commit the still-open transaction");
    assert_eq!(
        rows_of(db.execute("SELECT * FROM t").expect("select")),
        vec![vec![Value::Integer(1)]]
    );
}

#[test]
fn explain_verbose_includes_both_plans_plain_explain_only_the_physical_one() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let mut db = open(&dir);
    db.execute("CREATE TABLE t (a INTEGER)").expect("create table");

    let plain = plan_lines(db.execute("EXPLAIN SELECT * FROM t").expect("explain")).join("\n");
    assert!(!plain.contains("Logical plan:"), "plain EXPLAIN showed a logical plan:\n{plain}");
    assert!(!plain.contains("Physical plan:"), "plain EXPLAIN labeled its single tree:\n{plain}");

    let verbose =
        plan_lines(db.execute("EXPLAIN VERBOSE SELECT * FROM t").expect("explain verbose"))
            .join("\n");
    assert!(verbose.contains("Logical plan:"), "expected a logical plan header:\n{verbose}");
    assert!(verbose.contains("Physical plan:"), "expected a physical plan header:\n{verbose}");
}

#[test]
fn compound_predicate_shows_index_cond_and_residual_filter() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let mut db = open(&dir);
    db.execute("CREATE TABLE users (id INTEGER, name TEXT, age INTEGER)").expect("create table");
    db.execute("CREATE INDEX users_age_idx ON users (age)").expect("create index");

    let lines = plan_lines(
        db.execute("EXPLAIN SELECT id, name, age FROM users WHERE age >= 21 AND name = 'ada'")
            .expect("explain"),
    );
    let plan = lines.join("\n");

    assert!(plan.contains("Filter"), "expected a residual Filter above the index scan:\n{plan}");
    assert!(
        plan.contains("age >= 21") && plan.contains("name = 'ada'"),
        "expected the full compound predicate in the residual Filter:\n{plan}"
    );
    assert!(plan.contains("Index Scan"), "expected an Index Scan:\n{plan}");
    assert!(
        plan.contains("Index Cond: age >= 21"),
        "expected the Index Cond to show only the indexable conjunct:\n{plan}"
    );
}
