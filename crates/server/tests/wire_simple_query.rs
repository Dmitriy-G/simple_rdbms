use std::sync::Arc;

use common::DbConfig;
use engine::Database;
use tokio::net::TcpListener;
use tokio_postgres::SimpleQueryMessage;

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

fn column_names(columns: &[tokio_postgres::SimpleColumn]) -> Vec<&str> {
    columns.iter().map(|c| c.name()).collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn create_insert_and_select_round_trip_over_simple_query() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("wire_simple_query.db");
    let db = Arc::new(Database::open(DbConfig::new(db_path)).expect("open database"));
    let client = connect(db).await.expect("connect to the listener");

    let create = client
        .simple_query("CREATE TABLE t (flag BOOLEAN, n INTEGER, name TEXT)")
        .await
        .expect("create table succeeds");
    assert!(matches!(create.as_slice(), [SimpleQueryMessage::CommandComplete(0)]));

    let insert = client
        .simple_query("INSERT INTO t VALUES (TRUE, 1, 'ada'), (FALSE, 2, 'bob')")
        .await
        .expect("insert succeeds");
    assert!(matches!(insert.as_slice(), [SimpleQueryMessage::CommandComplete(2)]));

    let select = client.simple_query("SELECT * FROM t").await.expect("select succeeds");
    let SimpleQueryMessage::RowDescription(columns) = &select[0] else {
        panic!("expected a RowDescription first, got {:?}", select[0]);
    };
    assert_eq!(column_names(columns), vec!["flag", "n", "name"]);

    let rows: Vec<&tokio_postgres::SimpleQueryRow> = select
        .iter()
        .filter_map(|m| match m {
            SimpleQueryMessage::Row(row) => Some(row),
            _ => None,
        })
        .collect();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].get("flag"), Some("t"));
    assert_eq!(rows[0].get("n"), Some("1"));
    assert_eq!(rows[0].get("name"), Some("ada"));
    assert_eq!(rows[1].get("flag"), Some("f"));
    assert_eq!(rows[1].get("n"), Some("2"));
    assert_eq!(rows[1].get("name"), Some("bob"));

    assert!(matches!(select.last(), Some(SimpleQueryMessage::CommandComplete(2))));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_empty_select_still_reports_its_row_description() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("wire_simple_query_empty.db");
    let db = Arc::new(Database::open(DbConfig::new(db_path)).expect("open database"));
    let client = connect(db).await.expect("connect to the listener");

    client.simple_query("CREATE TABLE t (a INTEGER)").await.expect("create table succeeds");

    let select = client.simple_query("SELECT * FROM t").await.expect("select succeeds");
    let SimpleQueryMessage::RowDescription(columns) = &select[0] else {
        panic!("expected a RowDescription first, got {:?}", select[0]);
    };
    assert_eq!(column_names(columns), vec!["a"]);
    assert!(matches!(select.as_slice(), [_, SimpleQueryMessage::CommandComplete(0)]));
}

#[tokio::test(flavor = "multi_thread")]
async fn begin_and_commit_succeed_over_simple_query() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("wire_simple_query_txn.db");
    let db = Arc::new(Database::open(DbConfig::new(db_path)).expect("open database"));
    let client = connect(db).await.expect("connect to the listener");

    client.simple_query("CREATE TABLE t (a INTEGER)").await.expect("create table succeeds");

    let begin = client.simple_query("BEGIN").await.expect("begin succeeds");
    assert!(matches!(begin.as_slice(), [SimpleQueryMessage::CommandComplete(0)]));

    client.simple_query("INSERT INTO t VALUES (1)").await.expect("insert succeeds");

    let commit = client.simple_query("COMMIT").await.expect("commit succeeds");
    assert!(matches!(commit.as_slice(), [SimpleQueryMessage::CommandComplete(0)]));

    let select = client.simple_query("SELECT * FROM t").await.expect("select succeeds");
    let rows = select.iter().filter(|m| matches!(m, SimpleQueryMessage::Row(_))).count();
    assert_eq!(rows, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn explain_returns_a_query_plan_over_simple_query() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("wire_simple_query_explain.db");
    let db = Arc::new(Database::open(DbConfig::new(db_path)).expect("open database"));
    let client = connect(db).await.expect("connect to the listener");

    client.simple_query("CREATE TABLE t (a INTEGER)").await.expect("create table succeeds");

    let explain = client.simple_query("EXPLAIN SELECT * FROM t").await.expect("explain succeeds");
    let SimpleQueryMessage::RowDescription(columns) = &explain[0] else {
        panic!("expected a RowDescription first, got {:?}", explain[0]);
    };
    assert_eq!(column_names(columns), vec!["QUERY PLAN"]);
    let plan_rows = explain.iter().filter(|m| matches!(m, SimpleQueryMessage::Row(_))).count();
    assert!(plan_rows >= 1, "expected at least one query plan line");
}
