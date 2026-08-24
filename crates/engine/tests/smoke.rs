#![allow(clippy::unwrap_used, clippy::expect_used)]

use common::DbConfig;
use engine::Database;

#[test]
fn database_opens_and_closes() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let config = DbConfig::new(dir.path().join("smoke.db"));

    let db = Database::open(config).expect("open database");
    db.close().expect("close database");
}
