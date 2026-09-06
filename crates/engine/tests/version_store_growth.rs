use std::error::Error;

use common::DbConfig;
use engine::Database;

const AUTOCOMMIT_ROW_COUNT: usize = 300;

#[test]
fn version_chains_stay_bounded_across_autocommit_inserts_and_an_open_transaction()
-> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let mut db1 = Database::open(DbConfig::new(dir.path().join("test.db")))?;
    db1.execute("CREATE TABLE t (a INTEGER)")?;

    for i in 0..AUTOCOMMIT_ROW_COUNT {
        db1.execute(&format!("INSERT INTO t VALUES ({i})"))?;
    }

    assert_eq!(
        db1.stats()?.version_chains,
        0,
        "with every autocommit statement its own transaction and none left open, the watermark \
         should have pruned every chain by the time the last one commits"
    );

    let mut db2 = db1.connect()?;
    db2.execute("BEGIN")?;
    for i in AUTOCOMMIT_ROW_COUNT..AUTOCOMMIT_ROW_COUNT + 20 {
        db2.execute(&format!("INSERT INTO t VALUES ({i})"))?;
    }

    assert!(
        db1.stats()?.version_chains > 0,
        "an open transaction's own uncommitted inserts must still have live chains"
    );

    db2.execute("COMMIT")?;

    assert_eq!(
        db1.stats()?.version_chains,
        0,
        "once the second session's transaction commits with nothing else active, its chains \
         must be pruned too"
    );
    Ok(())
}
