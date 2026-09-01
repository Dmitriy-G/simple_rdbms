use common::{DbConfig, Error, SqlState};
use engine::{Database, ResultSet};
use types::Value;

#[cfg(test)]
fn open(dir: &tempfile::TempDir) -> Database {
    let config = DbConfig::new(dir.path().join("test.db"));
    Database::open(config).expect("open database")
}

fn rows_and_columns_of(result: ResultSet) -> (Vec<String>, Vec<Vec<Value>>) {
    match result {
        ResultSet::Rows { columns, rows } => {
            (columns, rows.into_iter().map(|t| t.values().to_vec()).collect())
        }
        ResultSet::RowsAffected(n) => panic!("expected Rows, got RowsAffected({n})"),
        ResultSet::RolledBack => panic!("expected Rows, got RolledBack"),
    }
}

#[test]
fn create_insert_and_select_star_preserves_insertion_order() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let mut db = open(&dir);

    db.execute("CREATE TABLE t (a INTEGER, b INTEGER)").expect("create table");
    let inserted = db.execute("INSERT INTO t VALUES (1, 10), (2, 20), (3, 30)").expect("insert");
    assert_eq!(inserted, ResultSet::RowsAffected(3));

    let (columns, rows) = rows_and_columns_of(db.execute("SELECT * FROM t").expect("select"));
    assert_eq!(columns, vec!["a", "b"]);
    assert_eq!(
        rows,
        vec![
            vec![Value::Integer(1), Value::Integer(10)],
            vec![Value::Integer(2), Value::Integer(20)],
            vec![Value::Integer(3), Value::Integer(30)],
        ]
    );
}

#[test]
fn select_list_can_subset_and_reorder_columns() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let mut db = open(&dir);

    db.execute("CREATE TABLE t (a INTEGER, b INTEGER)").expect("create table");
    db.execute("INSERT INTO t VALUES (1, 10), (2, 20)").expect("insert");

    let (columns, rows) = rows_and_columns_of(db.execute("SELECT b, a FROM t").expect("select"));
    assert_eq!(columns, vec!["b", "a"]);
    assert_eq!(
        rows,
        vec![
            vec![Value::Integer(10), Value::Integer(1)],
            vec![Value::Integer(20), Value::Integer(2)],
        ]
    );
}

#[test]
fn where_clause_evaluates_eq_lt_and_or_and_excludes_null_predicate_rows() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let mut db = open(&dir);

    db.execute("CREATE TABLE t (a INTEGER, b INTEGER)").expect("create table");
    db.execute("INSERT INTO t VALUES (1, 10), (2, 20), (3, NULL)").expect("insert");

    let (_, rows) = rows_and_columns_of(
        db.execute("SELECT a FROM t WHERE (a = 1 OR b < 15) AND a > 0").expect("select"),
    );
    assert_eq!(rows, vec![vec![Value::Integer(1)]]);
}

#[test]
fn where_is_null_and_is_not_null_find_and_exclude_null_rows() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let mut db = open(&dir);

    db.execute("CREATE TABLE t (a INTEGER, b INTEGER)").expect("create table");
    db.execute("INSERT INTO t VALUES (1, 10), (2, 20), (3, NULL)").expect("insert");

    let (_, rows) =
        rows_and_columns_of(db.execute("SELECT a FROM t WHERE b IS NULL").expect("select"));
    assert_eq!(rows, vec![vec![Value::Integer(3)]]);

    let (_, rows) =
        rows_and_columns_of(db.execute("SELECT a FROM t WHERE b IS NOT NULL").expect("select"));
    assert_eq!(rows, vec![vec![Value::Integer(1)], vec![Value::Integer(2)]]);
}

#[test]
fn insert_spanning_multiple_pages_is_all_readable_back() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let mut db = open(&dir);

    db.execute("CREATE TABLE t (n INTEGER)").expect("create table");

    const ROW_COUNT: usize = 1000;
    let values: Vec<String> = (0..ROW_COUNT).map(|n| format!("({n})")).collect();
    let insert_sql = format!("INSERT INTO t VALUES {}", values.join(", "));
    let inserted = db.execute(&insert_sql).expect("insert");
    assert_eq!(inserted, ResultSet::RowsAffected(ROW_COUNT));

    let (_, rows) = rows_and_columns_of(db.execute("SELECT n FROM t").expect("select"));
    assert_eq!(rows.len(), ROW_COUNT);
}

#[test]
fn data_survives_close_and_reopen() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let path = dir.path().join("restart.db");

    {
        let mut db = Database::open(DbConfig::new(&path)).expect("open database");
        db.execute("CREATE TABLE t (a INTEGER, b INTEGER)").expect("create table");
        db.execute("INSERT INTO t VALUES (1, 10), (2, 20)").expect("insert");
        db.close().expect("close database");
    }

    let mut db = Database::open(DbConfig::new(&path)).expect("reopen database");
    let (columns, rows) = rows_and_columns_of(db.execute("SELECT * FROM t").expect("select"));
    assert_eq!(columns, vec!["a", "b"]);
    assert_eq!(
        rows,
        vec![
            vec![Value::Integer(1), Value::Integer(10)],
            vec![Value::Integer(2), Value::Integer(20)],
        ]
    );
}

#[test]
fn repeated_close_and_reopen_cycles_accumulate_tables_without_extra_page_growth() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let path = dir.path().join("cycles.db");
    const CYCLES: usize = 10;

    for i in 0..CYCLES {
        let mut db = Database::open(DbConfig::new(&path)).expect("open database");
        db.execute(&format!("CREATE TABLE t{i} (a INTEGER)")).expect("create table");
        db.close().expect("close database");
    }

    let db = Database::open(DbConfig::new(&path)).expect("reopen database");
    let expected_names: Vec<String> = (0..CYCLES).map(|i| format!("t{i}")).collect();
    assert_eq!(db.table_names(), expected_names);

    let file_len = std::fs::metadata(&path).expect("stat database file").len();
    let page_size = DbConfig::DEFAULT_PAGE_SIZE as u64;
    let expected_pages = 2 + CYCLES as u64;
    assert_eq!(
        file_len,
        expected_pages * page_size,
        "file grew by more than the pages actually allocated"
    );
}

#[test]
fn integer_bigint_and_double_columns_round_trip_their_own_value_variants() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let mut db = open(&dir);

    db.execute("CREATE TABLE t (a INTEGER, b BIGINT, c DOUBLE)").expect("create table");
    db.execute("INSERT INTO t VALUES (1, 9999999999, 1.5)").expect("insert");

    let (columns, rows) = rows_and_columns_of(db.execute("SELECT * FROM t").expect("select"));
    assert_eq!(columns, vec!["a", "b", "c"]);
    assert_eq!(rows, vec![vec![Value::Integer(1), Value::BigInt(9999999999), Value::Double(1.5)]]);
}

#[test]
fn where_clause_integer_literal_narrows_against_an_integer_column() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let mut db = open(&dir);

    db.execute("CREATE TABLE t (a INTEGER)").expect("create table");
    db.execute("INSERT INTO t VALUES (1), (2)").expect("insert");

    let (_, rows) = rows_and_columns_of(db.execute("SELECT a FROM t WHERE a = 1").expect("select"));
    assert_eq!(rows, vec![vec![Value::Integer(1)]]);
}

#[test]
fn oversized_literal_into_integer_column_is_an_out_of_range_error_not_a_type_mismatch() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let mut db = open(&dir);

    db.execute("CREATE TABLE t (a INTEGER)").expect("create table");
    let too_big = i64::from(i32::MAX) + 1;
    let err = db
        .execute(&format!("INSERT INTO t VALUES ({too_big})"))
        .expect_err("expected an out-of-range error");
    let message = err.to_string();
    assert!(message.contains('a'), "expected the error to name column 'a', got: {message}");
    assert!(
        !message.to_lowercase().contains("type mismatch"),
        "expected an out-of-range error, not a type mismatch, got: {message}"
    );
}

#[test]
fn comparing_integer_column_to_bigint_column_is_still_an_error() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let mut db = open(&dir);

    db.execute("CREATE TABLE t (a INTEGER, b BIGINT)").expect("create table");
    let result = db.execute("SELECT * FROM t WHERE a = b");
    assert!(result.is_err(), "expected an error comparing INTEGER to BIGINT, got {result:?}");
}

#[test]
fn oversized_integer_literal_is_a_lexer_error_quoting_the_text() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let mut db = open(&dir);

    db.execute("CREATE TABLE t (a INTEGER)").expect("create table");
    let err = db
        .execute("INSERT INTO t VALUES (99999999999999999999)")
        .expect_err("expected a lexer error");
    let message = err.to_string();
    assert!(
        message.contains("99999999999999999999"),
        "expected the error to quote the offending literal, got: {message}"
    );
}

#[test]
fn an_index_on_a_low_cardinality_column_survives_bulk_insert() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let mut db = open(&dir);

    db.execute("CREATE TABLE orders (id INTEGER, status INTEGER)").expect("create table");
    db.execute("CREATE INDEX idx_status ON orders (status)").expect("create index");
    for id in 0..1_000 {
        db.execute(&format!("INSERT INTO orders VALUES ({id}, 1)")).expect("insert");
    }

    let (_, rows) =
        rows_and_columns_of(db.execute("SELECT id FROM orders WHERE status = 1").expect("select"));
    assert_eq!(rows.len(), 1_000);
}

#[test]
fn select_from_nonexistent_table_is_an_error() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let mut db = open(&dir);

    let result = db.execute("SELECT * FROM missing");

    assert!(result.is_err(), "expected an error, got {result:?}");
}

#[test]
fn a_parse_error_carries_sqlstate_42601_and_its_byte_offset() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let mut db = open(&dir);

    let sql = "SELECT * FROM t WHERE a = @";
    let err = db.execute(sql).expect_err("malformed SQL must fail to parse");

    assert_eq!(err.sql_state(), SqlState::SYNTAX_ERROR);
    match err {
        Error::Syntax { offset, .. } => {
            assert_eq!(offset, sql.find('@').expect("query contains the offending '@'"));
        }
        other => panic!("expected Error::Syntax, got {other:?}"),
    }
}
