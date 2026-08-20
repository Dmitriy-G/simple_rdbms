//! Asserts the crate compiles and its main types can be constructed.

use types::{DataType, Tuple, Value};

#[test]
fn tuple_constructs_from_values() {
    let tuple =
        Tuple::new(vec![Value::Integer(1), Value::Varchar("hello".to_string()), Value::Null]);
    assert_eq!(tuple.values().len(), 3);
    assert!(tuple.values()[2].is_null());
}

#[test]
fn data_type_variants_construct() {
    let types = [
        DataType::Boolean,
        DataType::Integer,
        DataType::BigInt,
        DataType::Double,
        DataType::Varchar(255),
    ];
    assert_eq!(types.len(), 5);
}
