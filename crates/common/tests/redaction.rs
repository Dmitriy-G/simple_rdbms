use common::Error;

const SENTINEL: &str = "SENTINEL_VALUE_4711";

fn value_bearing_errors() -> Vec<Error> {
    vec![
        Error::Syntax { message: format!("unexpected token '{SENTINEL}'"), offset: 5 },
        Error::DatatypeMismatch { detail: format!("found value {SENTINEL}") },
        Error::NumericValueOutOfRange {
            column: "age".to_string(),
            value: SENTINEL.to_string(),
            data_type: "INTEGER".to_string(),
        },
    ]
}

#[test]
fn redacted_never_contains_the_sentinel_value() {
    for err in value_bearing_errors() {
        let redacted = err.redacted();
        assert!(!redacted.contains(SENTINEL), "redacted() leaked a user value: {redacted:?}");
    }
}

#[test]
fn redacted_keeps_schema_identifiers() {
    let err = Error::NumericValueOutOfRange {
        column: "age".to_string(),
        value: SENTINEL.to_string(),
        data_type: "INTEGER".to_string(),
    };
    assert_eq!(err.redacted(), "value ? out of range for column age (INTEGER)");
}

#[test]
fn redacted_keeps_non_value_bearing_variants_unchanged() {
    let err = Error::UndefinedTable { name: "widgets".to_string() };
    assert_eq!(err.redacted(), err.to_string());
}
