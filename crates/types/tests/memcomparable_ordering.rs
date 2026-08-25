use std::cmp::Ordering;

use proptest::prelude::*;
use proptest::strategy::BoxedStrategy;
use types::{DataType, MemcomparableEncode, Value};

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

proptest! {
    #[test]
    fn encoded_byte_order_matches_value_compare((a, b) in arb_same_type_pair()) {
        let mut encoded_a = Vec::new();
        a.encode_memcomparable(&mut encoded_a);
        let mut encoded_b = Vec::new();
        b.encode_memcomparable(&mut encoded_b);

        let value_order = a.compare(&b).unwrap().unwrap_or(Ordering::Equal);
        prop_assert_eq!(encoded_a.cmp(&encoded_b), value_order);
    }

    #[test]
    fn null_sorts_before_every_non_null_value(v in arb_non_null_pair_for(DataType::Integer).prop_map(|(a, _)| a)) {
        let mut encoded_null = Vec::new();
        Value::Null.encode_memcomparable(&mut encoded_null);
        let mut encoded_v = Vec::new();
        v.encode_memcomparable(&mut encoded_v);

        prop_assert_eq!(encoded_null.cmp(&encoded_v), Ordering::Less);
    }
}
