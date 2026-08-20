//! Nothing in the SQL/planner/executor pipeline is wired up yet. This test
//! pins that down: `execute` must fail loudly with `Error::NotSupported`
//! rather than silently returning an empty or wrong result.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use common::{DbConfig, Error};
use engine::Database;

#[test]
fn select_1_returns_not_supported() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let config = DbConfig::new(dir.path().join("test.db"));
    let mut db = Database::open(config).expect("open database");

    let result = db.execute("SELECT 1");

    assert!(
        matches!(result, Err(Error::NotSupported(_))),
        "expected Error::NotSupported, got {result:?}"
    );
}
