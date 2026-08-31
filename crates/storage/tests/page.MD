# page

Unit-style coverage for `storage::page`'s pure checksum arithmetic
(`checksum_of`, `stamp_checksum`, `checksum_ok`), driven through the
public API rather than a `#[cfg(test)] mod tests` block since all three
are already `pub` and need no access to anything private
(CLAUDE.md's testing-rules carve-out is for private pure functions only).
`crates/storage/tests/disk_manager.rs` covers the same checksum at the
file layer - corruption detected after a real read - this file isolates
the arithmetic itself, with no `DiskManager` or temp file involved.

## Key Components

- `an_untouched_all_zero_page_reads_as_checksum_ok` - the invariant
  `CLAUDE.md`'s "all-zero pages are valid" rule depends on: a page that
  was allocated but never written must decode as valid, not corrupt, so
  `checksum_ok` special-cases the all-zero buffer before comparing any
  stored checksum.
- `a_freshly_stamped_page_reads_as_checksum_ok` - the ordinary path:
  `stamp_checksum` writes a checksum a subsequent `checksum_ok` call
  accepts.
- `a_flipped_bit_anywhere_in_the_payload_fails_the_checksum`/
  `a_flipped_bit_in_the_stored_checksum_itself_fails_the_checksum` - a
  single bit flip, in the covered payload or in the checksum field
  itself, is always caught.
- `checksum_of_ignores_the_checksum_field_itself` - `checksum_of` covers
  `CHECKSUM_RANGE.end..` only; overwriting the checksum field's own bytes
  before recomputing must not change the result, otherwise `stamp_checksum`
  would need two passes to converge.

## Usage Example

```rust
use storage::page::{PAGE_SIZE, stamp_checksum, checksum_ok};

let mut bytes = [0u8; PAGE_SIZE];
bytes[100] = 0xAB;
stamp_checksum(&mut bytes);
assert!(checksum_ok(&bytes));
```

Run via `cargo test -p storage --test page`. `storage::disk::DiskManager`
calls `checksum_ok` on every page read and `stamp_checksum` on every page
write (`disk.MD`); this file exercises that pair of functions directly,
without the file I/O `disk_manager.rs`'s tests need to prove the same
checks actually run on the real read/write path.
