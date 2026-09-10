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

#[tokio::test(flavor = "multi_thread")]
async fn set_show_and_reset_are_accepted_and_leave_the_connection_usable() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("wire_set_show_reset.db");
    let db = Arc::new(Database::open(DbConfig::new(db_path)).expect("open database"));
    let client = connect(db).await.expect("connect to the listener");

    let set = client
        .simple_query("SET extra_float_digits = 3")
        .await
        .expect("SET is accepted as a no-op");
    assert!(matches!(set.last(), Some(SimpleQueryMessage::CommandComplete(_))));

    let reset = client
        .simple_query("RESET extra_float_digits")
        .await
        .expect("RESET is accepted as a no-op");
    assert!(matches!(reset.last(), Some(SimpleQueryMessage::CommandComplete(_))));

    let show = client
        .simple_query("SHOW server_version")
        .await
        .expect("SHOW of a known parameter succeeds");
    let SimpleQueryMessage::Row(row) = &show[1] else {
        panic!("expected a Row second, got {:?}", show[1]);
    };
    assert!(row.get("server_version").is_some_and(|v| !v.is_empty()));

    let show_unknown = client
        .simple_query("SHOW nonexistent_parameter")
        .await
        .expect("SHOW of an unknown parameter succeeds with an empty string");
    let SimpleQueryMessage::Row(row) = &show_unknown[1] else {
        panic!("expected a Row second, got {:?}", show_unknown[1]);
    };
    assert_eq!(row.get("nonexistent_parameter"), Some(""));

    client
        .simple_query("CREATE TABLE t (a INTEGER)")
        .await
        .expect("the connection is still usable after SET/SHOW/RESET");
    let select = client.simple_query("SELECT * FROM t").await.expect("select succeeds");
    assert!(matches!(select.last(), Some(SimpleQueryMessage::CommandComplete(0))));
}

#[tokio::test(flavor = "multi_thread")]
async fn set_show_and_reset_keywords_are_matched_case_insensitively() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("wire_set_show_reset_case.db");
    let db = Arc::new(Database::open(DbConfig::new(db_path)).expect("open database"));
    let client = connect(db).await.expect("connect to the listener");

    client.simple_query("set extra_float_digits = 3").await.expect("lowercase SET is accepted");
    client.simple_query("Reset extra_float_digits").await.expect("mixed-case RESET is accepted");
}
