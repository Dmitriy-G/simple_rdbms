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

fn row_count(messages: &[SimpleQueryMessage]) -> usize {
    messages.iter().filter(|m| matches!(m, SimpleQueryMessage::Row(_))).count()
}

#[tokio::test(flavor = "multi_thread")]
async fn pg_class_lists_a_created_table() {
    let dir = tempfile::tempdir().expect("tempdir");
    let client = open(&dir, "wire_pg_catalog_pg_class.db").await;

    client.simple_query("CREATE TABLE t (a INTEGER)").await.expect("create table succeeds");

    let result = client
        .simple_query("SELECT oid, relname, relnamespace, relkind FROM pg_class")
        .await
        .expect("pg_class query succeeds");
    let row = only_row(&result);
    assert_eq!(row.get("relname"), Some("t"));
    assert_eq!(row.get("relkind"), Some("r"));
    assert_eq!(row.get("relnamespace"), Some("2200"));
}

#[tokio::test(flavor = "multi_thread")]
async fn pg_class_filtered_by_a_missing_relname_returns_zero_rows() {
    let dir = tempfile::tempdir().expect("tempdir");
    let client = open(&dir, "wire_pg_catalog_pg_class_missing.db").await;

    client.simple_query("CREATE TABLE t (a INTEGER)").await.expect("create table succeeds");

    let result = client
        .simple_query("SELECT relname FROM pg_catalog.pg_class WHERE relname = 'missing'")
        .await
        .expect("pg_class query succeeds");
    assert_eq!(row_count(&result), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn pg_namespace_lists_public_and_pg_catalog() {
    let dir = tempfile::tempdir().expect("tempdir");
    let client = open(&dir, "wire_pg_catalog_pg_namespace.db").await;

    let result = client
        .simple_query("SELECT oid, nspname FROM pg_namespace")
        .await
        .expect("pg_namespace query succeeds");
    let names: Vec<&str> = result
        .iter()
        .filter_map(|m| match m {
            SimpleQueryMessage::Row(row) => row.get("nspname"),
            _ => None,
        })
        .collect();
    assert_eq!(names.len(), 2);
    assert!(names.contains(&"public"));
    assert!(names.contains(&"pg_catalog"));
}

#[tokio::test(flavor = "multi_thread")]
async fn pg_class_reports_the_same_oid_for_a_table_across_two_queries() {
    let dir = tempfile::tempdir().expect("tempdir");
    let client = open(&dir, "wire_pg_catalog_pg_class_oid_stable.db").await;

    client.simple_query("CREATE TABLE t (a INTEGER)").await.expect("create table succeeds");

    let first = client
        .simple_query("SELECT oid FROM pg_class WHERE relname = 't'")
        .await
        .expect("first pg_class query succeeds");
    let second = client
        .simple_query("SELECT oid FROM pg_class WHERE relname = 't'")
        .await
        .expect("second pg_class query succeeds");

    let first_oid = only_row(&first).get("oid").expect("expected an oid column");
    let second_oid = only_row(&second).get("oid").expect("expected an oid column");
    assert_eq!(first_oid, second_oid);
}

#[tokio::test(flavor = "multi_thread")]
async fn pg_attribute_lists_columns_for_a_table_filtered_by_oid_in_attnum_order() {
    let dir = tempfile::tempdir().expect("tempdir");
    let client = open(&dir, "wire_pg_catalog_pg_attribute.db").await;

    client
        .simple_query("CREATE TABLE t (flag BOOLEAN, n INTEGER, name TEXT)")
        .await
        .expect("create table succeeds");

    let class_result = client
        .simple_query("SELECT oid FROM pg_class WHERE relname = 't'")
        .await
        .expect("pg_class query succeeds");
    let oid = only_row(&class_result).get("oid").expect("expected an oid column").to_string();

    let attr_result = client
        .simple_query(&format!(
            "SELECT attname, atttypid, attnum FROM pg_attribute WHERE attrelid = {oid}"
        ))
        .await
        .expect("pg_attribute query succeeds");
    let rows: Vec<&tokio_postgres::SimpleQueryRow> = attr_result
        .iter()
        .filter_map(|m| match m {
            SimpleQueryMessage::Row(row) => Some(row),
            _ => None,
        })
        .collect();
    assert_eq!(rows.len(), 3);

    assert_eq!(rows[0].get("attname"), Some("flag"));
    assert_eq!(rows[0].get("attnum"), Some("1"));
    assert_eq!(rows[0].get("atttypid"), Some(PgType::BOOL.oid().to_string()).as_deref());

    assert_eq!(rows[1].get("attname"), Some("n"));
    assert_eq!(rows[1].get("attnum"), Some("2"));
    assert_eq!(rows[1].get("atttypid"), Some(PgType::INT4.oid().to_string()).as_deref());

    assert_eq!(rows[2].get("attname"), Some("name"));
    assert_eq!(rows[2].get("attnum"), Some("3"));
    assert_eq!(rows[2].get("atttypid"), Some(PgType::VARCHAR.oid().to_string()).as_deref());
}

#[tokio::test(flavor = "multi_thread")]
async fn pg_type_reports_int4_and_zero_rows_for_an_unknown_typname() {
    let dir = tempfile::tempdir().expect("tempdir");
    let client = open(&dir, "wire_pg_catalog_pg_type.db").await;

    let int4 = client
        .simple_query("SELECT oid, typname FROM pg_type WHERE typname = 'int4'")
        .await
        .expect("pg_type query succeeds");
    let row = only_row(&int4);
    assert_eq!(row.get("typname"), Some("int4"));
    assert_eq!(row.get("oid"), Some(PgType::INT4.oid().to_string()).as_deref());

    let unknown = client
        .simple_query("SELECT oid FROM pg_type WHERE typname = 'lo'")
        .await
        .expect("pg_type query succeeds");
    assert_eq!(row_count(&unknown), 0);
}
