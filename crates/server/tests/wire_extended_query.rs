use std::sync::Arc;

use common::DbConfig;
use engine::Database;
use tokio::net::TcpListener;
use tokio_postgres::SimpleQueryMessage;
use tokio_postgres::types::Type as PgType;

async fn connect(db: Arc<Database>) -> Result<tokio_postgres::Client, Box<dyn std::error::Error>> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;

    tokio::spawn(server::wire::serve(listener, db));

    let conn_str =
        format!("host={} port={} user=simple_rdbms dbname=simple_rdbms", addr.ip(), addr.port());
    let (client, connection) = tokio_postgres::connect(&conn_str, tokio_postgres::NoTls).await?;
    tokio::spawn(async move {
        let _ = connection.await;
    });
    Ok(client)
}

#[cfg(test)]
async fn open(dir: &tempfile::TempDir, name: &str) -> tokio_postgres::Client {
    let db_path = dir.path().join(name);
    let db = Arc::new(Database::open(DbConfig::new(db_path)).expect("open database"));
    connect(db).await.expect("connect to the listener")
}

fn row_count(messages: &[SimpleQueryMessage]) -> usize {
    messages.iter().filter(|m| matches!(m, SimpleQueryMessage::Row(_))).count()
}

#[tokio::test(flavor = "multi_thread")]
async fn prepared_select_binds_i32_i64_f64_str_and_bool_parameters() {
    let dir = tempfile::tempdir().expect("tempdir");
    let client = open(&dir, "wire_extended_types.db").await;

    client
        .simple_query("CREATE TABLE t (flag BOOLEAN, n INTEGER, big BIGINT, d DOUBLE, name TEXT)")
        .await
        .expect("create table succeeds");
    client
        .simple_query("INSERT INTO t VALUES (TRUE, 7, 123456789012, 3.5, 'ada')")
        .await
        .expect("insert succeeds");

    let stmt = client
        .prepare("SELECT flag, n, big, d, name FROM t WHERE flag = $1 AND n = $2 AND big = $3 AND d = $4 AND name = $5")
        .await
        .expect("prepare succeeds");

    let rows = client
        .query(&stmt, &[&true, &7i32, &123456789012i64, &3.5f64, &"ada"])
        .await
        .expect("query with bound parameters succeeds");
    assert_eq!(rows.len(), 1);
    assert!(rows[0].get::<_, bool>("flag"));
    assert_eq!(rows[0].get::<_, i32>("n"), 7);
    assert_eq!(rows[0].get::<_, i64>("big"), 123456789012);
    assert_eq!(rows[0].get::<_, f64>("d"), 3.5);
    assert_eq!(rows[0].get::<_, &str>("name"), "ada");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_null_parameter_stores_a_null() {
    let dir = tempfile::tempdir().expect("tempdir");
    let client = open(&dir, "wire_extended_null.db").await;

    client
        .simple_query("CREATE TABLE t (flag BOOLEAN, name TEXT)")
        .await
        .expect("create table succeeds");

    let stmt = client.prepare("INSERT INTO t (name) VALUES ($1)").await.expect("prepare succeeds");
    let affected = client
        .execute(&stmt, &[&Option::<&str>::None])
        .await
        .expect("execute with a NULL parameter succeeds");
    assert_eq!(affected, 1);

    let select = client.simple_query("SELECT name FROM t").await.expect("select succeeds");
    let row = select
        .iter()
        .find_map(|m| match m {
            SimpleQueryMessage::Row(row) => Some(row),
            _ => None,
        })
        .expect("expected exactly one row");
    assert_eq!(row.get("name"), None, "the inserted NULL parameter must read back as SQL NULL");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_prepared_statement_runs_twice_with_different_values() {
    let dir = tempfile::tempdir().expect("tempdir");
    let client = open(&dir, "wire_extended_reuse.db").await;

    client.simple_query("CREATE TABLE t (a INTEGER)").await.expect("create table succeeds");

    let stmt = client.prepare("INSERT INTO t VALUES ($1)").await.expect("prepare succeeds");
    client.execute(&stmt, &[&1i32]).await.expect("first execution succeeds");
    client.execute(&stmt, &[&2i32]).await.expect("second execution succeeds");

    let select = client.simple_query("SELECT a FROM t").await.expect("select succeeds");
    let values: Vec<&str> = select
        .iter()
        .filter_map(|m| match m {
            SimpleQueryMessage::Row(row) => row.get("a"),
            _ => None,
        })
        .collect();
    assert_eq!(values, vec!["1", "2"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn begin_and_commit_wrap_a_prepared_insert() {
    let dir = tempfile::tempdir().expect("tempdir");
    let client = open(&dir, "wire_extended_txn.db").await;

    client.simple_query("CREATE TABLE t (a INTEGER)").await.expect("create table succeeds");

    client.simple_query("BEGIN").await.expect("begin succeeds");
    let stmt = client.prepare("INSERT INTO t VALUES ($1)").await.expect("prepare succeeds");
    client.execute(&stmt, &[&42i32]).await.expect("insert inside the transaction succeeds");
    client.simple_query("COMMIT").await.expect("commit succeeds");

    let select = client.simple_query("SELECT a FROM t").await.expect("select succeeds");
    assert_eq!(row_count(&select), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_wrong_typed_parameter_is_reported_as_an_error_not_a_dropped_connection() {
    let dir = tempfile::tempdir().expect("tempdir");
    let client = open(&dir, "wire_extended_wrong_type.db").await;

    client.simple_query("CREATE TABLE t (n INTEGER)").await.expect("create table succeeds");
    client.simple_query("INSERT INTO t VALUES (1)").await.expect("insert succeeds");

    let stmt = client
        .prepare_typed("SELECT n FROM t WHERE n = $1", &[PgType::VARCHAR])
        .await
        .expect("prepare succeeds regardless of the declared type disagreeing with inference");

    let err = client
        .query(&stmt, &[&"not-a-number"])
        .await
        .expect_err("a non-numeric value bound against an inferred Integer parameter must fail");
    let db_error = err.as_db_error().expect("expected a database error, not a connection failure");
    assert_eq!(db_error.code().code(), "08P01");

    let select = client.simple_query("SELECT n FROM t").await.expect("connection still usable");
    assert_eq!(row_count(&select), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_portal_suspends_and_resumes_across_fetch_limited_executes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut client = open(&dir, "wire_extended_portal_suspend.db").await;

    client.simple_query("CREATE TABLE t (a INTEGER)").await.expect("create table succeeds");
    for value in 1..=5i32 {
        client.execute("INSERT INTO t VALUES ($1)", &[&value]).await.expect("insert succeeds");
    }

    let txn = client.transaction().await.expect("begin transaction succeeds");
    let stmt = txn.prepare("SELECT a FROM t").await.expect("prepare succeeds");
    let portal = txn.bind(&stmt, &[]).await.expect("bind succeeds");

    let first = txn.query_portal(&portal, 2).await.expect("first fetch succeeds");
    let second = txn.query_portal(&portal, 2).await.expect("second fetch succeeds");
    let third = txn.query_portal(&portal, 2).await.expect("third fetch succeeds");

    assert_eq!(first.len(), 2);
    assert_eq!(second.len(), 2);
    assert_eq!(third.len(), 1);

    let values: Vec<i32> = first
        .iter()
        .chain(second.iter())
        .chain(third.iter())
        .map(|row| row.get::<_, i32>("a"))
        .collect();
    assert_eq!(values, vec![1, 2, 3, 4, 5]);

    txn.commit().await.expect("commit succeeds");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_fetch_limit_larger_than_the_row_count_completes_in_one_execute() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut client = open(&dir, "wire_extended_portal_no_suspend.db").await;

    client.simple_query("CREATE TABLE t (a INTEGER)").await.expect("create table succeeds");
    for value in 1..=5i32 {
        client.execute("INSERT INTO t VALUES ($1)", &[&value]).await.expect("insert succeeds");
    }

    let txn = client.transaction().await.expect("begin transaction succeeds");
    let stmt = txn.prepare("SELECT a FROM t").await.expect("prepare succeeds");
    let portal = txn.bind(&stmt, &[]).await.expect("bind succeeds");

    let rows = txn.query_portal(&portal, 10).await.expect("fetch succeeds");
    let values: Vec<i32> = rows.iter().map(|row| row.get::<_, i32>("a")).collect();
    assert_eq!(values, vec![1, 2, 3, 4, 5]);

    txn.commit().await.expect("commit succeeds");
}
