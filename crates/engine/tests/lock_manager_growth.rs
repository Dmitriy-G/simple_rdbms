use std::error::Error;

use common::DbConfig;
use engine::Database;

const AUTOCOMMIT_ROW_COUNT: usize = 300;
const AUTOCOMMIT_ROW_COUNT_WHILE_OPEN: usize = 20;

#[test]
fn finished_txn_set_stays_bounded_across_autocommit_statements_and_an_open_transaction()
-> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let mut db1 = Database::open(DbConfig::new(dir.path().join("test.db")))?;
    db1.execute("CREATE TABLE t (a INTEGER)")?;

    for i in 0..AUTOCOMMIT_ROW_COUNT {
        db1.execute(&format!("INSERT INTO t VALUES ({i})"))?;
    }

    assert_eq!(
        db1.stats()?.finished_txn_count,
        0,
        "with every autocommit statement its own transaction and none left open, the watermark \
         should have pruned every finished entry by the time the last one commits"
    );

    let mut db2 = db1.connect()?;
    db2.execute("BEGIN")?;

    for i in AUTOCOMMIT_ROW_COUNT..AUTOCOMMIT_ROW_COUNT + AUTOCOMMIT_ROW_COUNT_WHILE_OPEN {
        db1.execute(&format!("INSERT INTO t VALUES ({i})"))?;
    }

    assert!(
        db1.stats()?.finished_txn_count > 0,
        "autocommit transactions that finish while an older transaction is still open must stay \
         in the finished set until the watermark passes them"
    );

    db2.execute("COMMIT")?;

    assert_eq!(
        db1.stats()?.finished_txn_count,
        0,
        "once the older transaction commits with nothing else active, every finished entry \
         behind its watermark must be pruned too"
    );
    Ok(())
}
