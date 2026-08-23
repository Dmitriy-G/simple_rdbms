# disk_manager

Pages written through a `DiskManager`, then the file closed and reopened
from scratch, must read back with the same contents. Also covers every
clear-error path `DiskManager::open`/`read_page` are responsible for:
mismatched page size, an incompatible on-disk format version, a
non-page-aligned file length, an unallocated page read, and every flavor
of checksum corruption.

## Key Components

- `pages_survive_close_and_reopen` - writes two pages with distinct
  marker bytes at offset 20 (past the checksum/`page_lsn` prefixes
  `write_page` itself reserves and stamps/reads), syncs, drops the
  `DiskManager`, reopens, and reads both back.
- `reopening_with_a_different_page_size_is_a_clear_error` - opens with
  the default page size, then reopens the same file requesting double
  that; the error message must name both the stored and requested sizes.
- `reopening_a_file_with_a_different_format_version_is_a_clear_error` -
  overwrites just the header's version field (bytes `20..24` - after page
  0's own 4-byte checksum, 8-byte `page_lsn` prefix, and 8-byte magic)
  with `999` to simulate a file written by an incompatible build; the
  failure mode this guards against is real - `heap::NO_NEXT_PAGE` and the
  heap page header's layout both changed once already without this check
  existing yet, and a file written under the old scheme was then
  misinterpreted deep in the heap/catalog layers as a request for a
  nonexistent page instead of failing here, at open time, with a
  diagnosable reason. `DiskManager::open` reads this raw, before any
  checksum verification, so it fails on the version check rather than
  `ChecksumMismatch`; the error message must name the stored (`999`)
  version.
- `reopening_a_file_whose_length_is_not_a_page_multiple_is_a_clear_error` -
  truncates the file to `2 * PAGE_SIZE - 10` bytes, simulating what would
  happen if `allocate_page`'s `set_len` were ever interrupted (it
  currently is not, being a single syscall, but the check must still
  hold for any other way a file could end up short); the error message
  must name both the actual and expected lengths.
- `reading_an_unallocated_page_is_an_error_not_zeros` - reading a page id
  past the file's extent must error, not silently synthesize zeros.
- `flip_byte_at(path, page_id, offset_in_page)` - flips every bit of the
  byte at `offset_in_page` within `page_id`'s slot in the database file,
  simulating single-byte disk corruption (media bit rot, a stray write
  from outside the engine) that happened after the page was durably
  written.
- `freshly_allocated_never_written_page_reads_back_clean` - a page
  allocated (extending the file) but never written back through
  `write_page` reads back as a clean, all-zero page rather than tripping
  checksum verification - the exception `read_page` documents for the
  `NO_NEXT_PAGE` sentinel: a CRC-32 over 4092 zero bytes is not zero, so
  without this exception every freshly-allocated page would fail
  verification the first time anything read it back.
- `flipped_byte_in_the_header_region_is_a_checksum_mismatch` - a byte
  flipped just past the checksum and `page_lsn` prefix (offset 12, the
  slotted-page header) is caught on the next read, naming the offending
  page id.
- `flipped_byte_in_the_tuple_payload_region_is_a_checksum_mismatch` - a
  byte flipped near the end of the page (where tuple payload bytes are
  packed) is caught the same way - the checksum covers the whole page,
  not just its structured fields.
- `flipped_byte_inside_the_checksum_field_is_detected` - a byte flipped
  inside the checksum field itself (bytes `0..4`) is detected too: the
  stored checksum no longer matches the value recomputed over the rest of
  the page.

## Usage Example

```rust
let mut disk = DiskManager::open(dir.path().join("test.db"), PAGE_SIZE)?;
let page_id = disk.allocate_page()?;
let mut page = Page::new(page_id);
page.data_mut()[20..25].copy_from_slice(b"hello");
disk.write_page(page_id, &page)?;
disk.sync()?;
```

Run via `cargo test -p storage --test disk_manager`, driving
`storage::disk::DiskManager` directly against real temp-dir files - the
layer everything else in `storage` builds its own crash-safety on top of.
