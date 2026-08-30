# mod

Shared test-only helpers for `storage`'s own `tests/` integration suite,
declared with `mod support;` by whichever test file needs them. Nothing
here is gated behind `#[cfg(test)]` in `src/` — that attribute only takes
effect when compiling the crate's own unit-test binary, so it is invisible
to `tests/`, which links against an ordinary (non-`--cfg test`) build of
the crate. A helper shared with integration tests has to live in a real
module under `tests/` instead, which is what this one is; see
CLAUDE.md's testing section for when `#[cfg(test)]` in `src/` is still the
right call (only when the test needs access to a private item).

## Key Components

- `CountingDevice` - a `storage::block_device::BlockDevice` that wraps a
  real one and counts `read_at` calls and the cumulative bytes requested
  across them, via counters shared with the test that constructed it.
  Lets a test assert that `LogManager::read_at` costs a small, bounded
  amount of I/O per lookup rather than re-reading the whole log, without
  resorting to a flaky wall-clock timing assertion. `CountingDevice::new(inner,
  calls, bytes)` wraps `inner`, recording every `read_at` call's count and
  byte size into `calls`/`bytes`. Formerly lived behind `#[cfg(test)]` in
  `crates/storage/src/block_device.rs`; moved here because it implements
  only the public `BlockDevice` trait and touches nothing private, so the
  gate served no purpose beyond hiding it from `tests/`.
- `CountingSegmentStore` - a `storage::wal::SegmentStore` that wraps a real
  `FileSegmentStore` and hands every segment it opens through a
  `CountingDevice`, so a test can measure per-lookup I/O at the
  `SegmentStore` level the same way `CountingDevice` measures it at the
  `BlockDevice` level. `CountingSegmentStore::new(inner, calls, bytes, opens)`
  additionally counts `open` calls into `opens` - `wal_segments.rs`'s tests
  only care about `calls`/`bytes` and pass a throwaway counter for `opens`,
  while `undo_performance.rs`'s cross-segment test asserts on `opens`
  directly, to prove a backward undo walk across several sealed segments
  opens each one at most once rather than once per record undone.
  `existing_segments`/`remove` just forward to `inner` unchanged. Used to
  be two separate, near-identical private copies (one per file); unified
  here once both needed the same shape, since `mod support;` already
  existed for `CountingDevice` and the note below about it not crossing
  crate boundaries applies just as well to a second type in the same
  module.

## Usage Example

```rust
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

use storage::wal::{FileSegmentStore, LogManager};

mod support;
use support::CountingSegmentStore;

let calls = Arc::new(AtomicUsize::new(0));
let bytes = Arc::new(AtomicUsize::new(0));
let opens = Arc::new(AtomicUsize::new(0));
let store = Arc::new(CountingSegmentStore::new(
    FileSegmentStore::new(wal_path),
    calls.clone(),
    bytes.clone(),
    opens,
));
let log = LogManager::open_with_segment_store(store, target_segment_size)?;
```

`LogManager` always opens a `SegmentStore`, never a single device directly
(`wal.MD`'s single-on-disk-layout invariant), which is why measuring I/O
at the segment-open granularity needs a `SegmentStore` wrapper rather than
handing `CountingDevice` to `LogManager` directly.
`crates/storage/tests/wal_segments.rs` and
`crates/storage/tests/undo_performance.rs` both declare `mod support;` and
drive `CountingSegmentStore` this way (`mod support;` only resolves within
one crate's `tests/` directory, so this module cannot be shared with
another crate's tests - the note that motivated moving `CountingDevice`
here still applies, it just doesn't force a second copy of
`CountingSegmentStore` too), resetting `calls`/`bytes`/`opens` right before
the operation under measurement so setup I/O doesn't pollute the count.
