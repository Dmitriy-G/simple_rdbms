# tuple_roundtrip

Property-based round-trip test: `Tuple::decode` must reconstruct exactly
what `Encode::encode` produced, for any schema and matching set of values
(including `NULL`s in any column). Where `smoke.rs` only checks that the
types construct, this is the test that actually exercises `tuple.rs`'s
on-disk format against itself across a wide space of generated inputs,
using `proptest` (already a dev-dependency of every crate that needs
property tests, so no new dependency is added here).

## Key Components

- `arb_data_type() -> impl Strategy<Value = DataType>` - a `proptest`
  strategy generating one of the five `DataType` variants, with
  `Varchar`'s length capped at 64 to keep generated cases small and fast.
- `arb_value_for(data_type: DataType) -> BoxedStrategy<Value>` - a
  strategy generating a `Value` for a specific `DataType`, weighted 1:4 to
  also occasionally generate `Value::Null` regardless of the requested
  type (any column can hold `NULL`). `Double`'s generator explicitly
  filters out `NaN`, since `NaN != NaN` would make the round-trip
  assertion's equality check fail even on a perfectly correct encode/
  decode.
- `arb_schema_and_values() -> impl Strategy<Value = (Vec<DataType>, Vec<Value>)>` -
  builds a strategy yielding `(schema, values)` pairs where `values[i]`
  always matches `schema[i]`'s type (or is `Null`), by folding
  `arb_value_for` over a generated `Vec<DataType>` one column at a time
  so the two vectors are generated in lockstep rather than independently
  (independent generation could pair a schema and a values vector whose
  types don't line up at all).
- `tuple_roundtrips_through_encode_decode` - the property itself: encodes
  a `Tuple` built from generated `values`, decodes the resulting bytes
  against the matching generated `schema`, and asserts the decoded values
  equal the originals exactly.

## Usage Example

```rust
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
```

Run as part of `cargo test --workspace` like any other test; `proptest`
drives it with (by default) hundreds of randomly generated `(schema,
values)` pairs per run, shrinking any failing case down to a minimal
reproduction automatically, so a real encode/decode bug surfaces as a
small, readable counterexample rather than one specific hand-picked case
either passing or not.
