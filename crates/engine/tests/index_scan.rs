use common::DbConfig;
use engine::{Database, ResultSet};
use types::Value;

#[cfg(test)]
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
fn index_survives_close_and_reopen_after_a_root_split() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let path = dir.path().join("restart.db");
    const ROW_COUNT: i32 = 3_000;

    {
        let mut db = Database::open(DbConfig::new(&path)).expect("open database");
        db.execute("CREATE TABLE t (id INTEGER)").expect("create table");
        db.execute("CREATE INDEX idx_t_id ON t (id)").expect("create index");

        let values: Vec<String> = (0..ROW_COUNT).map(|n| format!("({n})")).collect();
        let insert_sql = format!("INSERT INTO t VALUES {}", values.join(", "));
        let inserted = db.execute(&insert_sql).expect("insert");
        assert_eq!(inserted, ResultSet::RowsAffected(ROW_COUNT as usize));
        db.close().expect("close database");
    }

    let mut db = Database::open(DbConfig::new(&path)).expect("reopen database");
    let rows = rows_of(db.execute("SELECT id FROM t WHERE id = 1500").expect("select"));
    assert_eq!(rows, vec![vec![Value::Integer(1500)]]);

    let rows = rows_of(db.execute("SELECT id FROM t WHERE id >= 0").expect("select all"));
    assert_eq!(
        rows.len(),
        ROW_COUNT as usize,
        "every row must survive the reopen through the index"
    );
}

#[test]
fn null_valued_indexed_column_is_never_returned_by_equality_or_range_predicates() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let mut db = open(&dir);

    db.execute("CREATE TABLE t (a INTEGER)").expect("create table");
    db.execute("CREATE INDEX idx_t_a ON t (a)").expect("create index");
    db.execute("INSERT INTO t VALUES (1), (2), (NULL)").expect("insert");

    let eq_rows = rows_of(db.execute("SELECT a FROM t WHERE a = 1").expect("eq select"));
    assert_eq!(eq_rows, vec![vec![Value::Integer(1)]]);

    let gt_rows = rows_of(db.execute("SELECT a FROM t WHERE a > 0").expect("gt select"));
    assert_eq!(
        gt_rows.len(),
        2,
        "a NULL indexed value must never satisfy a comparison predicate, got {gt_rows:?}"
    );
    assert!(gt_rows.iter().all(|row| row[0] != Value::Null));
}

#[test]
fn create_index_on_a_nonempty_table_populates_it() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let mut db = open(&dir);

    db.execute("CREATE TABLE t (a INTEGER)").expect("create table");
    db.execute("INSERT INTO t VALUES (1), (2), (3)").expect("insert");
    db.execute("CREATE INDEX idx_t_a ON t (a)").expect("create index on nonempty table");

    let rows = rows_of(db.execute("SELECT a FROM t WHERE a = 2").expect("select"));
    assert_eq!(rows, vec![vec![Value::Integer(2)]]);
}

#[test]
fn a_rolled_back_create_index_leaves_the_name_reusable() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let mut db = open(&dir);

    db.execute("CREATE TABLE t (a INTEGER)").expect("create table");
    db.execute("BEGIN").expect("begin");
    db.execute("CREATE INDEX idx_t_a ON t (a)").expect("create index");
    db.execute("ROLLBACK").expect("rollback");

    db.execute("CREATE INDEX idx_t_a ON t (a)")
        .expect("the rolled-back index's name must be reusable, proving it was actually removed");
}
