//! Property-based round-trip test: `Tuple::decode` must reconstruct exactly
//! what `Encode::encode` produced, for any schema and matching set of
//! values (including `NULL`s in any column).

use proptest::prelude::*;
use proptest::strategy::BoxedStrategy;
use types::{DataType, Encode, Tuple, Value};

fn arb_data_type() -> impl Strategy<Value = DataType> {
    prop_oneof![
        Just(DataType::Boolean),
        Just(DataType::Integer),
        Just(DataType::BigInt),
        Just(DataType::Double),
        (0u32..64).prop_map(DataType::Varchar),
    ]
}

fn arb_value_for(data_type: DataType) -> BoxedStrategy<Value> {
    let non_null: BoxedStrategy<Value> = match data_type {
        DataType::Boolean => any::<bool>().prop_map(Value::Boolean).boxed(),
        DataType::Integer => any::<i32>().prop_map(Value::Integer).boxed(),
        DataType::BigInt => any::<i64>().prop_map(Value::BigInt).boxed(),
        DataType::Double => {
            any::<f64>().prop_filter("no NaN", |f| !f.is_nan()).prop_map(Value::Double).boxed()
        }
        DataType::Varchar(_) => "[a-zA-Z0-9 ]{0,16}".prop_map(Value::Varchar).boxed(),
    };
    prop_oneof![1 => Just(Value::Null), 4 => non_null].boxed()
}

/// Builds a strategy yielding `(schema, values)` pairs where `values[i]`
/// always matches `schema[i]`'s type (or is `Null`).
fn arb_schema_and_values() -> impl Strategy<Value = (Vec<DataType>, Vec<Value>)> {
    proptest::collection::vec(arb_data_type(), 0..8).prop_flat_map(|schema| {
        let values_strategy = schema.iter().cloned().fold(
            Just(Vec::new()).boxed() as BoxedStrategy<Vec<Value>>,
            |acc, data_type| {
                (acc, arb_value_for(data_type))
                    .prop_map(|(mut values, value)| {
                        values.push(value);
                        values
                    })
                    .boxed()
            },
        );
        (Just(schema), values_strategy)
    })
}

proptest! {
    #[test]
    fn tuple_roundtrips_through_encode_decode((schema, values) in arb_schema_and_values()) {
        let tuple = Tuple::new(values.clone());
        let mut buf = Vec::new();
        tuple.encode(&mut buf);

        let decoded = Tuple::decode(&buf, &schema)?;
        prop_assert_eq!(decoded.values(), values.as_slice());
    }
}
