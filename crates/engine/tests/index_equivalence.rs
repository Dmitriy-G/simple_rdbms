use common::DbConfig;
use engine::{Database, ResultSet};
use proptest::prelude::*;
use proptest::test_runner::RngSeed;
use types::Value;

#[cfg(test)]
fn open(dir: &tempfile::TempDir) -> Database {
    let config = DbConfig::new(dir.path().join("test.db"));
    Database::open(config).expect("open database")
}

#[cfg(test)]
fn build_db(dir: &tempfile::TempDir, values: &[Option<i64>], with_index: bool) -> Database {
    let mut db = open(dir);
    db.execute("CREATE TABLE t (a BIGINT)").expect("create table");
    if with_index {
        db.execute("CREATE INDEX idx_t_a ON t (a)").expect("create index");
    }
    if !values.is_empty() {
        let rows: Vec<String> = values
            .iter()
            .map(|v| match v {
                Some(n) => format!("({n})"),
                None => "(NULL)".to_string(),
            })
            .collect();
        let sql = format!("INSERT INTO t VALUES {}", rows.join(", "));
        db.execute(&sql).expect("insert");
    }
    db
}

#[cfg(test)]
fn query_results(db: &mut Database, predicate: &str) -> Vec<Option<i64>> {
    let sql = format!("SELECT a FROM t WHERE {predicate}");
    let result = db.execute(&sql).expect("select");
    let ResultSet::Rows { rows, .. } = result else {
        panic!("expected Rows from a SELECT, got {result:?}");
    };
    let mut values: Vec<Option<i64>> = rows
        .into_iter()
        .map(|tuple| match tuple.values()[0] {
            Value::BigInt(n) => Some(n),
            Value::Null => None,
            ref other => panic!("expected a BigInt or NULL column, got {other:?}"),
        })
        .collect();
    values.sort_unstable();
    values
}

#[cfg(test)]
fn op_strategy() -> impl Strategy<Value = &'static str> {
    prop_oneof![Just("="), Just("<"), Just("<="), Just(">"), Just(">=")]
}

#[cfg(test)]
fn predicate_strategy() -> impl Strategy<Value = String> {
    let single = (op_strategy(), -20..20i64).prop_map(|(op, v)| format!("a {op} {v}"));
    let compound = (op_strategy(), -20..20i64, op_strategy(), -20..20i64)
        .prop_map(|(op1, v1, op2, v2)| format!("a {op1} {v1} AND a {op2} {v2}"));
    prop_oneof![2 => single, 1 => compound]
}

#[cfg(test)]
fn values_strategy() -> impl Strategy<Value = Vec<Option<i64>>> {
    proptest::collection::vec(proptest::option::of(-20..20i64), 0..40)
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 64,
        rng_seed: RngSeed::Fixed(0x5eed_5eed),
        ..ProptestConfig::default()
    })]

    #[test]
    fn index_scan_and_seq_scan_agree_on_every_matching_row(
        values in values_strategy(),
        predicate in predicate_strategy(),
    ) {
        let dir_indexed = tempfile::tempdir().unwrap();
        let dir_plain = tempfile::tempdir().unwrap();
        let mut db_indexed = build_db(&dir_indexed, &values, true);
        let mut db_plain = build_db(&dir_plain, &values, false);

        let indexed_results = query_results(&mut db_indexed, &predicate);
        let plain_results = query_results(&mut db_plain, &predicate);
        prop_assert_eq!(indexed_results, plain_results);
    }
}
