# devices

Every fault-injecting or call-counting wrapper the test suites build
around `storage::block_device::BlockDevice`/`storage::wal::SegmentStore`,
moved here from what used to be seven copies of `open_file`, two copies
of `faulty_devices`/`DeviceTriple`, a `FaultySegmentStore` gated
`#[cfg(feature = "test-util")]` inside `storage::wal` for no
reason it needed to live in the library at all, and a
`crates/storage/tests/support/mod.rs` holding `CountingDevice`/
`CountingSegmentStore` that only `storage`'s own `tests/` could reach.

## Key Components

- `DeviceTriple` - `(Box<dyn BlockDevice>, Arc<dyn SegmentStore>, Box<dyn
  BlockDevice>)`: the data-file device, WAL segment store, and
  double-write-buffer device a crash-injection test needs to open a
  database through `storage::disk::DiskManager::open_with_device`/
  `storage::wal::LogManager::open_with_segment_store`/
  `storage::dwb::DoubleWriteBuffer::open_with_device` (or, for `engine`,
  `engine::Database::open_with_devices`).
- `open_file(path) -> io::Result<File>` - `OpenOptions::new().read(true)
  .write(true).create(true).truncate(false).open(path)`, byte-identical
  across every one of its seven former call sites.
- `faulty_devices(dir, counter, fail_at, model) -> Result<DeviceTriple,
  _>` - builds a `DeviceTriple` over real files at `dir/test.db`,
  `dir/test.db.wal`, and `dir/test.db.dwb`, each wrapped so the `counter`
  shared `Arc<AtomicU64>` ticks on every device write attempt and the
  `fail_at`-th tick (under `model`) injects the fault
  `storage::block_device::DurabilityModel` describes - `write_is_durable`,
  `requires_sync`, `torn_write`, or `torn_write_requires_sync`. Byte-for-byte
  the same body as its two former copies (`crates/storage/tests/btree_crash_injection.rs`,
  `crates/engine/tests/crash_injection.rs`).
- `FaultySegmentStore` - wraps a `storage::wal::FileSegmentStore` so every
  segment it opens comes back as a `storage::block_device::FaultyDevice`
  sharing one fault counter and fail point; used by `faulty_devices` for
  the WAL leg of the triple. Was a `#[cfg(any(test, feature =
  "test-util"))]` item inside `storage::wal` even though it is composed
  entirely of `storage`'s own public types - it never needed to live in
  the library, and moving it here is what let that gate disappear
  entirely rather than merely narrow (`crates/storage/src/wal.MD`).
- `CountingDevice`/`CountingSegmentStore` - the non-faulting counterpart:
  every read call and byte read through a `CountingDevice` increments
  shared `Arc<AtomicUsize>` counters instead of ever failing, and
  `CountingSegmentStore` counts segment `open` calls the same way. Used
  by `crates/storage/tests/wal_segments.rs` and
  `crates/storage/tests/undo_performance.rs` to assert a bounded number of
  segment reads/opens rather than to test fault recovery.

## Usage Example

```rust
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use storage::block_device::DurabilityModel;
use test_support::faulty_devices;

let dir = tempfile::tempdir()?;
let counter = Arc::new(AtomicU64::new(0));
let (db_device, wal_store, dwb_device) =
    faulty_devices(dir.path(), &counter, 5, DurabilityModel::torn_write())?;
```

`test_support::crash::assert_workload_is_crash_safe` (`crash.MD`) is the
main caller: it sweeps `fail_at` across every value `counter` can reach
under a fault-free run, calling `faulty_devices` fresh for each sweep
point so no state leaks between crash simulations.
