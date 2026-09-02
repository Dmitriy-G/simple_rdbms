# lib

The crate root: re-exports every module's public surface so a consuming
`tests/*.rs` file can write `use test_support::{open_pool, PoolOptions, ...}`
without naming the submodule. `test-support` exists purely to end the
copy-paste duplication `docs/ROADMAP.md`'s testing-convention review
(`../../../.claude/task.MD`'s T-2) found across `storage`, `catalog`, `executor`, `txn`,
and `engine`'s own `tests/` directories: a dozen-plus near-identical
`open_pool`/`open_file`/`config`/`faulty_devices` functions, two
independent copies of the crash-injection sweep algorithm, and a
`tests/support/mod.rs` in two crates that could not share with each
other or with the crates that had no `tests/support/` at all. Every item
here is composed entirely of `storage`/`common`'s own public API - see
CLAUDE.md's dependency-edge rules for why this crate is a dev-dependency
everywhere it's used rather than a normal one.

## Key Components

- `crash` (`crash.MD`) - the generic crash-injection sweep
  (`CrashWorkload`, `assert_workload_is_crash_safe`) that replaces the two
  independent copies in `storage`'s and `engine`'s own crash-injection
  test files.
- `db` (`db.MD`) - `db_config`, the one-line `common::DbConfig` builder
  every `engine`-level test used to write out by hand.
- `devices` (`devices.MD`) - `open_file`, `DeviceTriple`, `faulty_devices`,
  `FaultySegmentStore`, `CountingDevice`, `CountingSegmentStore`: every
  fault-injecting or call-counting `storage::block_device::BlockDevice`/
  `storage::wal::SegmentStore` wrapper the test suites build.
- `logging` (`logging.MD`) - `CaptureBuf`, `captured_events`,
  `set_capturing_subscriber`: the thread-local tracing capture rig
  `logging.rs`-style tests use to assert on log output where logging
  really is the behavior under test (CLAUDE.md's testing-rules section).
- `pool` (`pool.MD`) - `PoolOptions`, `open_pool`, `open_pool_at_path`: one
  parameterised way to stand up a `storage::buffer::BufferPool` over a
  temp-dir database, replacing the dozen call-site variants that only
  ever differed in replacer `k`, WAL segment size, or a substituted data
  device.

## Usage Example

```rust
use test_support::{PoolOptions, open_pool};

let dir = tempfile::tempdir()?;
let pool = open_pool(dir.path(), PoolOptions::new(8))?;
```

Every crate that added `test-support` as a `[dev-dependencies]` entry
uses it exactly this way from its own `tests/*.rs` files - there is no
`mod support;` involved, since (unlike the old per-crate `tests/support/`
modules) this is a real, separately-compiled crate reachable from every
consumer's `tests/` directory at once, catalog, executor and txn included,
not just the one crate whose `tests/` it happened to live under before.
