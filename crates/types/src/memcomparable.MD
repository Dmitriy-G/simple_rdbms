# memcomparable

An order-preserving byte encoding for `Value`: `a.encode_memcomparable(...)`
sorts bytewise (`Ord` on `[u8]`, the same comparison `storage::btree` uses on
raw key bytes) in exactly the order `Value::compare` would report for `a`
and `b` of the same type. This is a different encoding from `Encode`
(`tuple.MD`): `Encode` packs a value for compact on-disk storage and is
read back with the schema's help, while this module exists solely so a
`BTreeIndex`, which only ever compares two `&[u8]` keys with `<`, can stay
ignorant of `Value`/`DataType` entirely (see `storage::btree`'s module
doc) and still get correct ordering. Little-endian integers, `Encode`'s
choice, are not memcomparable — byte 0 of a little-endian `i32` is the
*least* significant byte, so `256i32` (`00 01 00 00`) would sort before
`2i32` (`02 00 00 00`) under plain byte comparison. Every encoding here
fixes that by construction.

## Key Components

- `MemcomparableEncode` - the trait. `encode_memcomparable(&self, buf: &mut
  Vec<u8>) -> Result<(), ValueError>` appends this value's order-preserving
  bytes to `buf`, or returns `ValueError::UnorderableValue` (`value.MD`)
  without touching `buf` if this value has no defined ordering. On error,
  `buf` is left exactly as it was passed in - nothing partial is appended -
  so a caller building a composite key by encoding several values in
  sequence can safely bail out on the first error without having to
  truncate `buf` back to its prior length itself.
- `impl MemcomparableEncode for Value` - the only implementation.
  `Value::Null` encodes as a single `0x00` byte, sorting before every
  non-null value regardless of type (a index only ever holds keys of one
  declared column type, so cross-type ordering is not a concern - the tag
  only needs to separate "no value" from "a value"). Every non-null
  variant is prefixed with `0x01` and then:
  - `Boolean` - one byte, `0` or `1`.
  - `Integer`/`BigInt` - the value's sign bit flipped, then written
    big-endian. Flipping the sign bit maps the signed range onto the
    unsigned range in order (`i32::MIN` becomes unsigned `0`, `i32::MAX`
    becomes unsigned `u32::MAX`), and big-endian byte order makes unsigned
    magnitude comparison equal to bytewise comparison.
  - `Double` - `order_preserving_bits` applied to the IEEE-754 bit
    pattern, then written big-endian: for non-negative floats (sign bit
    0) the sign bit is set, mapping them above every negative float; for
    negative floats every bit is flipped, which both clears the sign bit
    (keeping them below non-negative floats) and reverses the magnitude
    ordering of the remaining bits (IEEE-754 magnitude bits already sort
    correctly for positive floats, but sort backwards for negative ones
    without this flip). `-0.0` is normalized to `0.0` first - IEEE-754
    gives them different bit patterns despite `-0.0 == 0.0`, and
    `Value::compare` (via `f64::partial_cmp`) agrees they're equal, so
    without this normalization they would encode to different byte
    strings and this module's own ordering guarantee would not hold for
    that one pair of values (caught by
    `tests/memcomparable_ordering.rs`'s property test). `NaN` is rejected
    with `ValueError::UnorderableValue` rather than given a bit pattern:
    `f64::partial_cmp` (what `Value::compare` uses) already refuses to
    order NaN, returning `None`, so silently assigning it a position
    above positive infinity (as the raw bit-flip transform would) would
    let a NaN key be inserted into a `BTreeIndex` at a position `get`
    could never find it again by - a lookup that walks the tree via `<`
    comparisons would never revisit that slot. Rejecting it at encode
    time turns that into a clean, immediate error instead of a silently
    corrupted index.
  - `Varchar` - the string's raw UTF-8 bytes, unmodified. No length
    prefix or terminator - see "Concatenation is not safe" below for
    exactly what that does and does not make safe.
- `decode_memcomparable(bytes: &[u8], data_type: DataType) -> Result<(Value, usize), ValueError>` -
  the inverse of `encode_memcomparable`: reads one value of the given
  `data_type` off the front of `bytes` and returns it along with how many
  bytes it consumed. Every fixed-width `data_type` (`Boolean`,
  `Integer`, `BigInt`, `Double`) consumes exactly its known width plus the
  one-byte tag, reversing the bit transform described above; `Varchar` has
  no self-describing length (see below), so it consumes every remaining
  byte in `bytes` and the caller is responsible for handing it exactly one
  encoded value's worth. A `bytes` too short for the tag or the value it
  claims, an unrecognized tag byte, or (for `Varchar`) bytes that are not
  valid UTF-8 all return `ValueError::InvalidEncoding` rather than
  panicking. Exists for the index-only scans planned for a later
  milestone, and used today by `storage::btree::BTreeIndex::check_invariants`
  to render a decoded value instead of a raw byte array in its error
  strings when the caller can supply the key's `DataType`.
- `order_preserving_bits(v: f64) -> u64` - private. The bit-flip
  transform `Double` encoding uses, described above.
- `read_be_u32`/`read_be_u64` - private. Read a fixed-width big-endian
  integer off the front of a byte slice, returning `None` instead of
  panicking if the slice is too short - `decode_memcomparable`'s only use
  of raw slice indexing, kept inside these two helpers rather than
  `TryInto::try_into().expect(..)` so the crate stays free of
  `clippy::expect_used`/`unwrap_used`.

## Concatenation is not safe

This encoding is order-preserving for exactly one value compared against
another value of the *same* `DataType` - which is all `storage::btree`
needs, since one index key today is one encoded column value. It is
**not** safe to concatenate `encode(a) ++ encode(b)` and expect the result
to sort correctly as a composite key: `Varchar`'s encoding has no
terminator or length prefix precisely because nothing in this module ever
needs to find where one encoded value ends and the next begins - the
`u16` key-length prefix `storage::btree`'s leaf/internal entry format adds
around the *whole* encoded key (see `btree.MD`'s "Node layout") is what
delimits a single key within its slot, and that prefix says nothing about
where a boundary would fall *inside* a multi-value key. Without a
terminator, `encode(Varchar("a")) ++ encode(Varchar("bc"))` and
`encode(Varchar("ab")) ++ encode(Varchar("c"))` would produce ordering
that mixes the two conceptually-separate values' bytes together with no
way to tell them apart, breaking both correct comparison and correct
decoding. A future composite-key encoding will need to escape `0x00` as
`0x00 0xFF` and append a `0x00 0x00` terminator after each `Varchar`
component before concatenation, the standard memcomparable technique for
making a length-prefix-free encoding safe to concatenate - marked in
`memcomparable.rs` with `// TODO(M11): composite key encoding` at the
point in `MemcomparableEncode` where that escaping would need to happen.

## Usage Example

```rust
use types::{MemcomparableEncode, Value};

let mut low = Vec::new();
Value::Integer(-5).encode_memcomparable(&mut low)?;
let mut high = Vec::new();
Value::Integer(3).encode_memcomparable(&mut high)?;
assert!(low < high);
```

No caller in this workspace builds `BTreeIndex` keys yet - `catalog` and
`executor` still route every table through `heap::TableHeap` only (see
`storage::btree`'s module doc). This module exists so the eventual
index-build and index-scan code (tracked as milestone M9.3 in
`docs/ROADMAP.md`) has a fixed, already-correct way to turn a column
`Value` into a key `BTreeIndex::insert`/`get` can compare directly against
every other key in the same index.
