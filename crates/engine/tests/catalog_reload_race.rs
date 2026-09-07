use std::error::Error;
use std::sync::Barrier;
use std::thread;

use common::DbConfig;
use engine::{Database, ResultSet};
use types::Value;

const CREATE_ROUNDS: usize = 30;
const SPLIT_ROUNDS: usize = 300;
const SPLIT_BATCH: i64 = 1;

#[test]
fn a_committed_table_survives_a_concurrent_catalog_reload() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let db0 = Database::open(DbConfig::new(dir.path().join("test.db")))?;
    let mut creator = db0.connect()?;
    let mut roller = db0.connect()?;

    for round in 0..CREATE_ROUNDS {
        let create_sql = format!("CREATE TABLE t{round} (a INTEGER)");
        let barrier = Barrier::new(2);
        let creator_ref = &mut creator;
        let create_sql_ref = &create_sql;
        let creator_barrier = &barrier;
        let roller_ref = &mut roller;
        let roller_barrier = &barrier;
        thread::scope(|scope| -> Result<(), common::Error> {
            let create = scope.spawn(move || -> Result<ResultSet, common::Error> {
                creator_barrier.wait();
                creator_ref.execute(create_sql_ref)
            });
            let roll = scope.spawn(move || {
                roller_barrier.wait();
                let _ = roller_ref.execute("SELECT * FROM this_table_does_not_exist");
            });

            create.join().expect("creator thread must not panic")?;
            roll.join().expect("roller thread must not panic");
            Ok(())
        })?;
    }

    for round in 0..CREATE_ROUNDS {
        let table_name = format!("t{round}");
        assert!(
            db0.table_names().contains(&table_name),
            "table {table_name}, committed while a concurrent session's catalog reload was in \
             flight, must still be resolvable afterwards"
        );
    }
    Ok(())
}

#[test]
fn an_index_root_split_survives_a_concurrent_catalog_reload() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let mut db0 = Database::open(DbConfig::new(dir.path().join("test.db")))?;
    db0.execute("CREATE TABLE t (a BIGINT)")?;
    db0.execute("CREATE INDEX idx_t_a ON t (a)")?;

    let mut inserter = db0.connect()?;
    let mut roller = db0.connect()?;

    for round in 0..SPLIT_ROUNDS {
        let base = round as i64 * SPLIT_BATCH;
        let values: Vec<String> = (0..SPLIT_BATCH).map(|i| format!("({})", base + i)).collect();
        let insert_sql = format!("INSERT INTO t VALUES {}", values.join(", "));
        let barrier = Barrier::new(2);
        let inserter_ref = &mut inserter;
        let insert_sql_ref = &insert_sql;
        let inserter_barrier = &barrier;
        let roller_ref = &mut roller;
        let roller_barrier = &barrier;
        thread::scope(|scope| -> Result<(), common::Error> {
            let insert = scope.spawn(move || -> Result<ResultSet, common::Error> {
                inserter_barrier.wait();
                inserter_ref.execute(insert_sql_ref)
            });
            let roll = scope.spawn(move || {
                roller_barrier.wait();
                let _ = roller_ref.execute("SELECT * FROM this_table_does_not_exist");
            });

            insert.join().expect("inserter thread must not panic")?;
            roll.join().expect("roller thread must not panic");
            Ok(())
        })?;
    }

    let total = SPLIT_ROUNDS as i64 * SPLIT_BATCH;
    let result = db0.execute(&format!("SELECT a FROM t WHERE a >= 0 AND a < {total}"))?;
    let ResultSet::Rows { rows, .. } = result else {
        panic!("expected Rows from a SELECT, got {result:?}");
    };
    let mut seen: Vec<i64> = rows
        .into_iter()
        .map(|tuple| match tuple.values()[0] {
            Value::BigInt(n) => n,
            ref other => panic!("expected a BigInt column, got {other:?}"),
        })
        .collect();
    seen.sort_unstable();
    let expected: Vec<i64> = (0..total).collect();
    assert_eq!(
        seen, expected,
        "every value inserted across an index root split racing a concurrent catalog reload \
         must still be reachable through the index"
    );
    Ok(())
}
