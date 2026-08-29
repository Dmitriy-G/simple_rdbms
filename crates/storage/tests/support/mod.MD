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

## Usage Example

```rust
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

use storage::wal::{FileSegmentStore, SegmentStore};

mod support;
use support::CountingDevice;

struct CountingSegmentStore {
    inner: FileSegmentStore,
    calls: Arc<AtomicUsize>,
    bytes: Arc<AtomicUsize>,
}

impl SegmentStore for CountingSegmentStore {
    // existing_segments and remove just forward to `inner`
    fn open(&self, id: u64) -> Result<Box<dyn storage::block_device::BlockDevice>, storage::StorageError> {
        let device = self.inner.open(id)?;
        Ok(Box::new(CountingDevice::new(device, self.calls.clone(), self.bytes.clone())))
    }
}
```

`LogManager` always opens a `SegmentStore`, never a single device directly
(`wal.MD`'s single-on-disk-layout invariant), so wrapping one device in
`CountingDevice` means implementing a small `SegmentStore` like this one
that hands every segment it opens through `CountingDevice` first.
`crates/storage/tests/undo_performance.rs` declares `mod support;`, keeps
its own private copy of exactly this `CountingSegmentStore` (`mod support;`
only resolves within one crate's `tests/` directory, so it cannot be
shared further than that), and drives it this way, resetting
`calls`/`bytes` right before the operation under measurement so setup I/O
doesn't pollute the count.
