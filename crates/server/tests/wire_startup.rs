use std::sync::Arc;

use common::DbConfig;
use engine::Database;
use tokio::net::TcpListener;

#[tokio::test(flavor = "multi_thread")]
async fn a_tokio_postgres_client_completes_the_startup_handshake() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("wire_startup.db");
    let db = Database::open(DbConfig::new(db_path)).expect("open database");
    let db = Arc::new(db);

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind ephemeral port");
    let addr = listener.local_addr().expect("read local addr");

    tokio::spawn(server::wire::serve(listener, db));

    let conn_str =
        format!("host={} port={} user=simple_rdbms dbname=simple_rdbms", addr.ip(), addr.port());
    let (client, connection) = tokio_postgres::connect(&conn_str, tokio_postgres::NoTls)
        .await
        .expect("startup handshake completes");
    tokio::spawn(async move {
        let _ = connection.await;
    });

    drop(client);
}
