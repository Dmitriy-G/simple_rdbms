# block_device

The lowest layer both `DiskManager` and `LogManager` write through: a
small trait abstracting "a file-like byte range on stable storage,"
implemented for real by `FileDevice` and, for crash-injection testing, by
`FaultyDevice` (see `crates/engine/tests/crash_injection.rs`). Routing
every read/write through this trait rather than calling `std::fs::File`
directly is what lets a test wrap either file (the main database file or
the WAL) in a device that fails on cue, so the crash-injection harness can
simulate a process dying at an arbitrary point in the middle of a real
workload without actually killing a process.

## Key Components

- `BlockDevice` - the trait. `DiskManager` and `LogManager` are written
  entirely in terms of it, never `std::fs::File` directly.
  `read_at(offset, buf)` reads `buf.len()` bytes starting at `offset`,
  failing if fewer are available. `write_at(offset, buf)` writes `buf`
  starting at `offset`. `set_len(len)` truncates or extends the device to
  exactly `len` bytes. `sync_all()` forces every write made so far to
  durable storage. `size()` returns the device's current length in bytes
  (named `size` rather than `len` since a device isn't a collection - no
  `is_empty` makes sense alongside it).
- `FileDevice` - the real `BlockDevice`: an ordinary OS file.
  `FileDevice::new(file)` wraps an already-open file.
- `DurabilityModel` - how `FaultyDevice` treats a completed
  `write_at`/`set_len` call: does it land on the wrapped device
  immediately, or does it only become durable once a later `sync_all`
  succeeds - and, independently, is the specific call the fault trips
  itself torn (a real subset of its bytes lands) or lost outright? These
  two questions are orthogonal, which is why this is a struct of two
  `bool`s (`tear_on_fault`, `durable_only_on_sync`) rather than an enum of
  named combinations: `tear_on_fault` is about what a crash does to the
  *one* call it interrupts, while `durable_only_on_sync` is about what a
  crash does to every *other* call that already reported success but was
  never followed by a `sync_all`. Composing both is what a real power
  failure looks like - the write in flight when the power dies lands torn,
  and anything sitting in the page cache before it, unsynced, is gone -
  which is exactly the case the double-write buffer exists to survive;
  testing `tear_on_fault` only against a device that is otherwise
  perfectly durable never exercises that combination. Four named
  constructors provide the meaningful points in this 2x2 space:
  `write_is_durable()` (`false, false`) is the harness's original,
  stronger-than-real-disks model - only the one tripped write is lost,
  never anything sitting in a cache, covering a crash that lands between
  two syscalls. `requires_sync()` (`false, true`) additionally covers a
  crash or power loss that lands *after* a `write()` call returns but
  *before* the next `fsync()` - reads still see pending writes (matching
  real page-cache behavior), so only what a crash would lose differs from
  `write_is_durable`. `torn_write()` (`true, false`) is like
  `write_is_durable` for every call that doesn't trip the fault, but the
  tripped call itself is torn rather than lost - a seeded-random subset of
  its 512-byte sectors still lands, the failure mode M12's checksum and
  double-write buffer exist to detect and repair. `torn_write_requires_sync()`
  (`true, true`) composes both: the tripped call tears *and* everything
  else still needs a `sync_all`, which is what an actual power failure
  does and the specific combination the double-write buffer must survive
  even though neither of the other two alone exercises it.
- `FaultyDevice` - a `BlockDevice` that wraps a real one and fails the
  first mutating call (`write_at` or `set_len`) once a shared counter
  passes `fail_at`, simulating a crash mid-workload. Reads never fail.
  Whether a write that completed *before* the fault fired actually
  survives the "crash" depends on `model`. The counter is an
  `Rc<Cell<u64>>` so that two `FaultyDevice`s - one wrapping the database
  file, one wrapping the WAL - can share a single counter, making "fail at
  write N" count across the whole system rather than per file, which is
  what actually happens when a process dies. `FaultyDevice::new(inner,
  counter, fail_at)` wraps `inner` under `DurabilityModel::write_is_durable`.
  `FaultyDevice::with_model(inner, counter, fail_at, model)` wraps under an
  explicit model; when `model.tear_on_fault` is set, the landed sectors
  are a seeded-random subset derived from `fail_at`, so a sweep failure at
  a given fail point is reproducible from the failure message alone.
  `FaultyDevice::with_torn_sectors(inner, counter, fail_at, model, sectors)`
  pins the exact 512-byte sector indices that land when the tripped write
  tears, instead of deriving them from a seeded RNG - test-only, for
  targeting a specific torn shape (e.g. `vec![0]` for "only the first
  sector") rather than hoping a seed happens to produce it.
- `SECTOR_SIZE` - `512`, the sector size a real disk actually lands a
  write as: a 4KB page write reaches hardware as 8 of these, and a crash
  mid-write can land any subset of them rather than a clean prefix.
- `PendingOp` - private. A `write_at`/`set_len` call not yet covered by a
  `sync_all`, under a model with `durable_only_on_sync` set. `Write {
  offset, bytes }` or `SetLen(u64)`.
- `FaultyDevice::tick(&self) -> io::Result<()>` - private. Ticks the
  shared write counter, returning an injected error once it has exceeded
  `fail_at`.
- `FaultyDevice::materialized(&mut self) -> io::Result<Vec<u8>>` - private.
  The device's contents as a live process would currently see them:
  whatever's really durable on `inner`, with every still-pending
  write/length-change replayed on top, in order. Only ever called with a
  non-empty `pending` (which requires `model.durable_only_on_sync`) -
  `read_at`/`size` skip straight to `inner` otherwise, since there is
  nothing to overlay.
- `FaultyDevice::tear(&mut self, offset, buf)` - private. Lands the
  sectors of `buf` that a torn version of this call would actually reach
  the inner device, writing each landed sector directly - never through
  `pending` regardless of `model.durable_only_on_sync`, since a sector a
  crash physically wrote to the platter is durable by definition,
  independent of whether a later `fsync` was ever going to be needed for
  anything else. Never lands a sector that would extend `inner` past its
  current, already-durable length: growing a file is `set_len`'s job, not
  an incidental side effect of `write_at` seeking past the current end -
  under `durable_only_on_sync`, an unsynced `set_len` correctly has not
  applied yet, and letting a torn write's direct-to-`inner` sector land
  past that not-yet-durable boundary would extend the file anyway,
  leaving a length no `set_len` call ever actually produced (surfacing as
  `StorageError::TruncatedFile` on the next open - a different failure
  class than the one this model is for).
- `random_sector_subset(sector_count, seed) -> Vec<usize>` - private. A
  seeded-random, *proper* subset of `0..sector_count` - never empty and
  never the full set - deterministic from `seed` alone so a
  crash-injection sweep's failure at a given fail point is reproducible
  from the failure message (which already names the fail point) without
  recording anything else; `seed` is always the fail point itself.
  Excluding the full set matters beyond realism: `write_at` returning
  `Err` is this harness's *only* signal that a call didn't reliably land,
  and every caller (and every crash-injection assertion) relies on that -
  a write that lands in full despite reporting failure would be durable
  and undetectable as torn (no checksum or CRC would ever catch it),
  silently breaking that contract. A single-sector write (`sector_count ==
  1`, e.g. a small WAL record) has no subset between "nothing landed" and
  "it all landed" - there is no such thing as a partial write of less than
  one physical sector - so for it, tearing can only ever mean total loss.

## Usage Example

```rust
use std::cell::Cell;
use std::rc::Rc;

use storage::block_device::{DurabilityModel, FaultyDevice, FileDevice};

let counter = Rc::new(Cell::new(0));
let db_device = FaultyDevice::with_model(
    Box::new(FileDevice::new(db_file)),
    counter.clone(),
    7,
    DurabilityModel::torn_write_requires_sync(),
);
```

`crates/engine/tests/crash_injection.rs` builds a `FaultyDevice` per file
(database, WAL, double-write buffer) sharing one counter, sweeps every
possible fail point under all four `DurabilityModel` compositions, and
checks that whatever the recovered database ends up containing matches
replaying exactly the safely-committed prefix of the workload - this is
the harness M12's crash model is proven against.
