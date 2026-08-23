# crc

A hand-written, table-driven CRC-32 (the IEEE 802.3 polynomial, reflected
form — the same checksum used by zip, gzip, and Ethernet), used by the
write-ahead log to detect a torn or corrupted record. Deliberately no
external crate: the lookup table is built once, lazily, from the
polynomial itself, keeping `common` free of a dependency for something
this small and stable.

## Key Components

- `POLYNOMIAL` - the reflected IEEE 802.3 polynomial, `0xEDB88320` (the
  bit-reversal of the standard `0x04C11DB7` polynomial), used because
  CRC-32 is conventionally computed least-significant-bit first.
- `table() -> &'static [u32; 256]` - private. Lazily builds and caches the
  256-entry lookup table behind a `OnceLock`, so the table is computed at
  most once no matter how many times `crc32` is called.
- `crc32(data) -> u32` - computes the CRC-32 (IEEE 802.3) checksum of
  `data`.

## Usage Example

```rust
use common::crc::crc32;

let checksum = crc32(b"a write-ahead log record's bytes");
```

`storage`'s write-ahead log stamps this checksum into every record it
appends and recomputes it on read, so a record torn or corrupted by a
crash mid-write is detected rather than silently replayed.
