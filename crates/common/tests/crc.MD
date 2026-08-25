# crc

`common::crc::crc32` checked against known values, using only the
module's public function. Formerly an inline `#[cfg(test)] mod tests` in
`crates/common/src/crc.rs`; moved here since `crc32` is public and the
tests touch nothing private, per CLAUDE.md's testing section - an inline
`#[cfg(test)]` module in `src/` is only justified when a test needs access
to something that should stay private.

## Key Components

- `empty_input_is_zero` - `crc32(b"")` must be `0x00000000`, the identity
  case for the algorithm's initial/final XOR.
- `known_vector_123456789` - `crc32(b"123456789")` must be `0xCBF43926`,
  the standard CRC-32/IEEE-802.3 check value used to verify a from-scratch
  implementation against every other conforming one.

## Usage Example

```rust
use common::crc::crc32;

#[test]
fn example_matches_the_standard_check_value() {
    assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
}
```

Run as part of `cargo test --workspace`, or just this crate with
`cargo test -p common --test crc`; `storage`'s write-ahead log is the
actual consumer of `crc32`, but recomputes and checks it directly against
record bytes rather than against `common`'s own tests.
