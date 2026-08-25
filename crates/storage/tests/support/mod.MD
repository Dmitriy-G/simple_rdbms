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
use std::cell::Cell;
use std::rc::Rc;

use storage::block_device::{BlockDevice, FileDevice};
use storage::wal::LogManager;

mod support;
use support::CountingDevice;

let calls = Rc::new(Cell::new(0));
let bytes = Rc::new(Cell::new(0));
let device: Box<dyn BlockDevice> =
    Box::new(CountingDevice::new(Box::new(FileDevice::new(wal_file)), calls.clone(), bytes.clone()));
let log = LogManager::open_with_device(device)?;
```

`crates/storage/tests/undo_performance.rs` is the one test that currently
declares `mod support;` and drives `CountingDevice` this way, resetting
`calls`/`bytes` right before the operation under measurement so setup I/O
doesn't pollute the count.
