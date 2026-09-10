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

#[cfg(test)]
async fn open(dir: &tempfile::TempDir, name: &str) -> tokio_postgres::Client {
    let db_path = dir.path().join(name);
    let db = Arc::new(Database::open(DbConfig::new(db_path)).expect("open database"));
    connect(db).await.expect("connect to the listener")
}

fn only_row(messages: &[SimpleQueryMessage]) -> &tokio_postgres::SimpleQueryRow {
    let rows: Vec<&tokio_postgres::SimpleQueryRow> = messages
        .iter()
        .filter_map(|m| match m {
            SimpleQueryMessage::Row(row) => Some(row),
            _ => None,
        })
        .collect();
    assert_eq!(rows.len(), 1, "expected exactly one row, got {messages:?}");
    rows[0]
}

#[tokio::test(flavor = "multi_thread")]
async fn select_version_lowercase_answers_a_postgresql_version_string() {
    let dir = tempfile::tempdir().expect("tempdir");
    let client = open(&dir, "wire_pg_catalog_version_lower.db").await;

    let result = client.simple_query("select version()").await.expect("select version succeeds");
    let row = only_row(&result);
    let version = row.get("version").expect("expected a version column");
    assert!(version.starts_with("PostgreSQL "), "unexpected version string: {version}");
}

#[tokio::test(flavor = "multi_thread")]
async fn select_version_uppercase_with_semicolon_answers_a_postgresql_version_string() {
    let dir = tempfile::tempdir().expect("tempdir");
    let client = open(&dir, "wire_pg_catalog_version_upper.db").await;

    let result = client.simple_query("SELECT VERSION();").await.expect("select version succeeds");
    let row = only_row(&result);
    let version = row.get("version").expect("expected a version column");
    assert!(version.starts_with("PostgreSQL "), "unexpected version string: {version}");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_ordinary_select_is_unaffected() {
    let dir = tempfile::tempdir().expect("tempdir");
    let client = open(&dir, "wire_pg_catalog_ordinary_select.db").await;

    client.simple_query("CREATE TABLE t (a INTEGER)").await.expect("create table succeeds");
    client.simple_query("INSERT INTO t VALUES (1)").await.expect("insert succeeds");

    let result = client.simple_query("SELECT a FROM t").await.expect("select succeeds");
    let row = only_row(&result);
    assert_eq!(row.get("a"), Some("1"));

    let err = client
        .simple_query("SELECT 1")
        .await
        .expect_err("a FROM-less SELECT must still be the engine's ordinary syntax error");
    let db_error = err.as_db_error().expect("expected a database error, not a connection failure");
    assert_eq!(db_error.code().code(), "42601");
}
