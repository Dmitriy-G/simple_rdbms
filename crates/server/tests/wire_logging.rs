use std::sync::Arc;

use common::DbConfig;
use engine::Database;
use test_support::{CaptureBuf, captured_events, set_global_capturing_subscriber};
use tokio::net::TcpListener;

#[cfg(test)]
async fn connect(db: Arc<Database>) -> tokio_postgres::Client {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind an ephemeral listener");
    let addr = listener.local_addr().expect("the listener has a local address");

    tokio::spawn(server::wire::serve(listener, db));

    let conn_str =
        format!("host={} port={} user=simple_rdbms dbname=simple_rdbms", addr.ip(), addr.port());
    let (client, connection) =
        tokio_postgres::connect(&conn_str, tokio_postgres::NoTls).await.expect("connect");
    tokio::spawn(async move {
        let _ = connection.await;
    });
    client
}

fn answered_events(capture: &CaptureBuf) -> Vec<serde_json::Value> {
    captured_events(capture)
        .into_iter()
        .filter(|event| {
            event["level"] == "INFO"
                && event["fields"]["message"] == "pgwire: answered without the engine"
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn every_query_the_wire_layer_answers_itself_is_logged_with_its_shape_and_row_count() {
    let capture = CaptureBuf::default();
    set_global_capturing_subscriber(&capture);

    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("wire_logging.db");
    let db = Arc::new(Database::open(DbConfig::new(db_path)).expect("open database"));
    let client = connect(db).await;

    client.simple_query("CREATE TABLE t (a INTEGER)").await.expect("create table");
    client.simple_query("SELECT relname FROM pg_class").await.expect("pg_class query");
    client.simple_query("SELECT indexrelid FROM pg_index").await.expect("pg_index query");
    client.simple_query("SHOW server_version").await.expect("show");
    client.simple_query("SELECT a FROM t").await.expect("an ordinary select");

    let events = answered_events(&capture);

    let pg_class = events
        .iter()
        .find(|event| event["fields"]["shape"] == "pg_class")
        .expect("the intercepted pg_class query was logged");
    assert_eq!(pg_class["fields"]["rows"], 1, "one table exists, so one row was answered");
    assert!(
        pg_class["fields"]["peer"].as_str().is_some_and(|peer| peer.contains("127.0.0.1")),
        "the log line must carry the peer address that correlates it with the accept line"
    );

    let unrecognized = events
        .iter()
        .find(|event| event["fields"]["shape"] == "unrecognized pg_ relation")
        .expect("the catch-all answer was logged");
    assert_eq!(
        unrecognized["fields"]["rows"], 0,
        "a zero-row answer is the fact the log exists to make visible"
    );

    assert!(
        events.iter().any(|event| event["fields"]["shape"] == "SHOW"),
        "SHOW is answered by the wire layer too, so it is logged too"
    );

    assert!(
        captured_events(&capture)
            .iter()
            .all(|event| { event["level"] != "INFO" || event["fields"]["sql"].is_null() }),
        "raw query text must not appear above debug"
    );

    assert!(
        !events.iter().any(|event| {
            event["fields"]["shape"] == "pg_class" && event["fields"]["rows"].is_null()
        }),
        "every answered event carries a row count"
    );
}
