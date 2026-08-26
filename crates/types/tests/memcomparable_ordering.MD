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
  given `DataType`. `Double`'s generator filters out `NaN`, since
  `encode_memcomparable` now rejects it (`encoding_nan_is_rejected` below
  covers that case directly instead), the same reasoning
  `tuple_roundtrip.rs` applies for a different reason.
- `arb_same_type_pair() -> impl Strategy<Value = (Value, Value)>` - picks a
  `DataType` uniformly, then generates a pair for it via
  `arb_non_null_pair_for`, so every generated pair is comparable under
  `Value::compare` (comparing across types is an error, not a case this
  property is about).
- `arb_non_null_value() -> impl Strategy<Value = Value>` - the same
  per-`DataType` generators as `arb_non_null_pair_for`, but a single value
  rather than a pair, for properties that only need one input (the decode
  round trip below).
- `encoded_byte_order_matches_value_compare` - the main property: encodes
  both generated values and asserts `encoded_a.cmp(&encoded_b)` equals
  `a.compare(&b)`'s `Ordering` (unwrapped, since neither operand is
  `Value::Null` here).
- `null_sorts_before_every_non_null_value` - the `Value::Null` case
  separately, since `Value::compare` returns `None` for any comparison
  involving `NULL` rather than an `Ordering` `memcomparable`'s NULL-first
  guarantee could be checked against directly.
- `decoding_round_trips_every_encoded_value` - encodes a generated value
  and feeds the bytes straight back into `decode_memcomparable` with the
  value's own `DataType`, asserting the decoded `Value` equals the
  original and that every byte was consumed - the property
  `check_invariants`'s decoded error messages (`storage::btree.MD`) and
  the index-only scans `decode_memcomparable` is meant for both depend on.
- `encoding_nan_is_rejected` - `Value::Double(f64::NAN)` returns
  `ValueError::UnorderableValue` and leaves the output buffer untouched,
  rather than silently encoding to a value `Value::compare` could never
  agree sorts where it lands.
- `negative_zero_and_positive_zero_encode_identically` - `-0.0` and `0.0`
  must produce the same bytes even though they have different IEEE-754
  bit patterns, since `Value::compare` already treats them as equal.
- `infinities_encode_and_order_correctly` - `f64::NEG_INFINITY` sorts
  below `0.0`, which sorts below `f64::INFINITY`, and both infinities
  round-trip through `decode_memcomparable` unchanged.
- `decoding_null_tag_yields_null_regardless_of_data_type` -
  `decode_memcomparable` reads the `Value::Null` tag byte before ever
  looking at `data_type`, so a `Null` decodes correctly no matter which
  `DataType` the caller passes.
- `decoding_an_empty_buffer_errors_cleanly` - an empty slice has no tag
  byte to read, so `decode_memcomparable` returns
  `ValueError::InvalidEncoding` rather than panicking.

## Usage Example

```rust
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
}
```

Run as part of `cargo test --workspace` like any other test; a failure
here means `storage::btree`'s `get`/`range_scan` would silently return
wrong or incomplete results for real column data once M9.3 wires this
encoding into the catalog and executor, even though `storage`'s own tests
(which only ever use hand-picked raw byte keys) would keep passing.
