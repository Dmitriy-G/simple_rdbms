# memcomparable_ordering

Property-based test that `memcomparable.rs`'s encoding is actually
order-preserving: for two same-type values, comparing their encoded byte
strings with `[u8]`'s own `Ord` must agree with `Value::compare`. This is
the property `storage::btree` depends on but cannot check itself, since it
never constructs a `Value` (see `storage::btree`'s module doc) - this test
is where that guarantee is actually verified, against every `DataType`
`Value` has, using `proptest`.

## Key Components

- `arb_non_null_pair_for(data_type: DataType) -> BoxedStrategy<(Value, Value)>` -
  a `proptest` strategy generating two same-type, non-null `Value`s for a
  given `DataType`. `Double`'s generator filters out `NaN`, since this
  encoding (deliberately, see `memcomparable.MD`) gives `NaN` no canonical
  ordering, the same reasoning `tuple_roundtrip.rs` applies for a
  different reason.
- `arb_same_type_pair() -> impl Strategy<Value = (Value, Value)>` - picks a
  `DataType` uniformly, then generates a pair for it via
  `arb_non_null_pair_for`, so every generated pair is comparable under
  `Value::compare` (comparing across types is an error, not a case this
  property is about).
- `encoded_byte_order_matches_value_compare` - the main property: encodes
  both generated values and asserts `encoded_a.cmp(&encoded_b)` equals
  `a.compare(&b)`'s `Ordering` (unwrapped, since neither operand is
  `Value::Null` here).
- `null_sorts_before_every_non_null_value` - the `Value::Null` case
  separately, since `Value::compare` returns `None` for any comparison
  involving `NULL` rather than an `Ordering` `memcomparable`'s NULL-first
  guarantee could be checked against directly.

## Usage Example

```rust
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
}
```

Run as part of `cargo test --workspace` like any other test; a failure
here means `storage::btree`'s `get`/`range_scan` would silently return
wrong or incomplete results for real column data once M9.3 wires this
encoding into the catalog and executor, even though `storage`'s own tests
(which only ever use hand-picked raw byte keys) would keep passing.
