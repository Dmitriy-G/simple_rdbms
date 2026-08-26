use std::cmp::Ordering;

use proptest::prelude::*;
use proptest::strategy::BoxedStrategy;
use types::{DataType, MemcomparableEncode, Value, ValueError, decode_memcomparable};

fn arb_non_null_pair_for(data_type: DataType) -> BoxedStrategy<(Value, Value)> {
    match data_type {
        DataType::Boolean => (any::<bool>(), any::<bool>())
            .prop_map(|(a, b)| (Value::Boolean(a), Value::Boolean(b)))
            .boxed(),
        DataType::Integer => (any::<i32>(), any::<i32>())
            .prop_map(|(a, b)| (Value::Integer(a), Value::Integer(b)))
            .boxed(),
        DataType::BigInt => (any::<i64>(), any::<i64>())
            .prop_map(|(a, b)| (Value::BigInt(a), Value::BigInt(b)))
            .boxed(),
        DataType::Double => (
            any::<f64>().prop_filter("no NaN", |f| !f.is_nan()),
            any::<f64>().prop_filter("no NaN", |f| !f.is_nan()),
        )
            .prop_map(|(a, b)| (Value::Double(a), Value::Double(b)))
            .boxed(),
        DataType::Varchar(_) => ("[a-zA-Z0-9 ]{0,32}", "[a-zA-Z0-9 ]{0,32}")
            .prop_map(|(a, b)| (Value::Varchar(a), Value::Varchar(b)))
            .boxed(),
    }
}

fn arb_same_type_pair() -> impl Strategy<Value = (Value, Value)> {
    prop_oneof![
        Just(DataType::Boolean),
        Just(DataType::Integer),
        Just(DataType::BigInt),
        Just(DataType::Double),
        Just(DataType::Varchar(32)),
    ]
    .prop_flat_map(arb_non_null_pair_for)
}

fn arb_non_null_value() -> impl Strategy<Value = Value> {
    prop_oneof![
        Just(DataType::Boolean),
        Just(DataType::Integer),
        Just(DataType::BigInt),
        Just(DataType::Double),
        Just(DataType::Varchar(32)),
    ]
    .prop_flat_map(|dt| arb_non_null_pair_for(dt).prop_map(|(a, _)| a))
}

proptest! {
    #[test]
    fn encoded_byte_order_matches_value_compare((a, b) in arb_same_type_pair()) {
        let mut encoded_a = Vec::new();
        a.encode_memcomparable(&mut encoded_a).unwrap();
        let mut encoded_b = Vec::new();
        b.encode_memcomparable(&mut encoded_b).unwrap();

        let value_order = a.compare(&b).unwrap().unwrap_or(Ordering::Equal);
        prop_assert_eq!(encoded_a.cmp(&encoded_b), value_order);
    }

    #[test]
    fn null_sorts_before_every_non_null_value(v in arb_non_null_pair_for(DataType::Integer).prop_map(|(a, _)| a)) {
        let mut encoded_null = Vec::new();
        Value::Null.encode_memcomparable(&mut encoded_null).unwrap();
        let mut encoded_v = Vec::new();
        v.encode_memcomparable(&mut encoded_v).unwrap();

        prop_assert_eq!(encoded_null.cmp(&encoded_v), Ordering::Less);
    }

    #[test]
    fn decoding_round_trips_every_encoded_value(v in arb_non_null_value()) {
        let data_type = v.data_type().unwrap();
        let mut encoded = Vec::new();
        v.encode_memcomparable(&mut encoded).unwrap();

        let (decoded, consumed) = decode_memcomparable(&encoded, data_type).unwrap();
        prop_assert_eq!(consumed, encoded.len());
        prop_assert_eq!(decoded, v);
    }
}

#[test]
fn encoding_nan_is_rejected() {
    let mut buf = Vec::new();
    let err = Value::Double(f64::NAN).encode_memcomparable(&mut buf).unwrap_err();
    assert!(matches!(err, ValueError::UnorderableValue { .. }));
    assert!(buf.is_empty());
}

#[test]
fn negative_zero_and_positive_zero_encode_identically() {
    let mut neg = Vec::new();
    Value::Double(-0.0).encode_memcomparable(&mut neg).unwrap();
    let mut pos = Vec::new();
    Value::Double(0.0).encode_memcomparable(&mut pos).unwrap();
    assert_eq!(neg, pos);
}

#[test]
fn infinities_encode_and_order_correctly() {
    let mut neg_inf = Vec::new();
    Value::Double(f64::NEG_INFINITY).encode_memcomparable(&mut neg_inf).unwrap();
    let mut zero = Vec::new();
    Value::Double(0.0).encode_memcomparable(&mut zero).unwrap();
    let mut pos_inf = Vec::new();
    Value::Double(f64::INFINITY).encode_memcomparable(&mut pos_inf).unwrap();

    assert!(neg_inf < zero);
    assert!(zero < pos_inf);

    let (decoded_neg, _) = decode_memcomparable(&neg_inf, DataType::Double).unwrap();
    assert_eq!(decoded_neg, Value::Double(f64::NEG_INFINITY));
    let (decoded_pos, _) = decode_memcomparable(&pos_inf, DataType::Double).unwrap();
    assert_eq!(decoded_pos, Value::Double(f64::INFINITY));
}

#[test]
fn decoding_null_tag_yields_null_regardless_of_data_type() {
    let mut encoded = Vec::new();
    Value::Null.encode_memcomparable(&mut encoded).unwrap();
    let (decoded, consumed) = decode_memcomparable(&encoded, DataType::Integer).unwrap();
    assert_eq!(decoded, Value::Null);
    assert_eq!(consumed, 1);
}

#[test]
fn decoding_an_empty_buffer_errors_cleanly() {
    let err = decode_memcomparable(&[], DataType::Integer).unwrap_err();
    assert!(matches!(err, ValueError::InvalidEncoding { .. }));
}
