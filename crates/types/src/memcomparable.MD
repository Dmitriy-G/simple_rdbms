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
  Vec<u8>)` appends this value's order-preserving bytes to `buf`.
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
    `tests/memcomparable_ordering.rs`'s property test). `NaN`'s several
    bit patterns are not given a canonical form; SQL does not define an
    ordering for `NaN` either, so this module does not need one.
  - `Varchar` - the string's raw UTF-8 bytes, unmodified. No length
    prefix or terminator: a `MemcomparableEncode` key is never
    concatenated with anything else inside `storage::btree` (one index
    key is exactly one encoded value), so there is nothing for a length
    prefix to disambiguate, and UTF-8's own byte order already matches
    Unicode codepoint order.
- `order_preserving_bits(v: f64) -> u64` - private. The bit-flip
  transform `Double` encoding uses, described above.

## Usage Example

```rust
use types::{MemcomparableEncode, Value};

let mut low = Vec::new();
Value::Integer(-5).encode_memcomparable(&mut low);
let mut high = Vec::new();
Value::Integer(3).encode_memcomparable(&mut high);
assert!(low < high);
```

No caller in this workspace builds `BTreeIndex` keys yet - `catalog` and
`executor` still route every table through `heap::TableHeap` only (see
`storage::btree`'s module doc). This module exists so the eventual
index-build and index-scan code (tracked as milestone M9.3 in
`docs/ROADMAP.md`) has a fixed, already-correct way to turn a column
`Value` into a key `BTreeIndex::insert`/`get` can compare directly against
every other key in the same index.
