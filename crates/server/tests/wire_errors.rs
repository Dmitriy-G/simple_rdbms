use std::sync::Arc;

use common::DbConfig;
use engine::Database;
use tokio::net::TcpListener;

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

fn sqlstate(err: &tokio_postgres::Error) -> Option<&str> {
    Some(err.as_db_error()?.code().code())
}

#[tokio::test(flavor = "multi_thread")]
async fn an_undefined_table_reports_the_real_sqlstate_and_leaves_the_connection_usable() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("wire_errors_undefined_table.db");
    let db = Arc::new(Database::open(DbConfig::new(db_path)).expect("open database"));
    let client = connect(db).await.expect("connect to the listener");

    let err = client
        .simple_query("SELECT * FROM missing")
        .await
        .expect_err("selecting from an undefined table fails");
    assert_eq!(sqlstate(&err), Some("42P01"));

    client
        .simple_query("CREATE TABLE t (a INTEGER)")
        .await
        .expect("the connection is still usable after an autocommit error");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_semicolon_terminated_begin_still_tracks_transaction_status() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("wire_errors_semicolon_begin.db");
    let db = Arc::new(Database::open(DbConfig::new(db_path)).expect("open database"));
    let client = connect(db).await.expect("connect to the listener");

    client.simple_query("CREATE TABLE t (a INTEGER);").await.expect("create table succeeds");
    client.simple_query("BEGIN;").await.expect("begin with a trailing semicolon succeeds");

    let first_err = client
        .simple_query("SELECT * FROM missing;")
        .await
        .expect_err("selecting from an undefined table fails");
    assert_eq!(sqlstate(&first_err), Some("42P01"));

    let second_err = client
        .simple_query("SELECT * FROM t;")
        .await
        .expect_err("a statement after an in-transaction error is rejected");
    assert_eq!(sqlstate(&second_err), Some("25P02"));

    client
        .simple_query("ROLLBACK;")
        .await
        .expect("rollback with a trailing semicolon succeeds even after the failure");

    client
        .simple_query("SELECT * FROM t;")
        .await
        .expect("the connection is usable again after rollback");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_statement_inside_a_transaction_fails_until_rollback() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("wire_errors_failed_transaction.db");
    let db = Arc::new(Database::open(DbConfig::new(db_path)).expect("open database"));
    let client = connect(db).await.expect("connect to the listener");

    client.simple_query("CREATE TABLE t (a INTEGER)").await.expect("create table succeeds");
    client.simple_query("BEGIN").await.expect("begin succeeds");

    let first_err = client
        .simple_query("SELECT * FROM missing")
        .await
        .expect_err("selecting from an undefined table fails");
    assert_eq!(sqlstate(&first_err), Some("42P01"));

    let second_err = client
        .simple_query("SELECT * FROM t")
        .await
        .expect_err("a statement after an in-transaction error is rejected");
    assert_eq!(sqlstate(&second_err), Some("25P02"));

    client
        .simple_query("ROLLBACK")
        .await
        .expect("rollback succeeds even though the transaction had already failed");

    client
        .simple_query("SELECT * FROM t")
        .await
        .expect("the connection is usable again after rollback");
}
