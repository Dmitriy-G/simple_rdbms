# test-support

Shared test-only fixtures for the workspace's `tests/` integration suites:
fault-injecting devices, buffer-pool setup, tracing capture, and the
crash-injection sweep algorithm. It exists to end the copy-paste
duplication a testing-convention review (`task.MD`'s T-2) found across
`storage`, `catalog`, `executor`, `txn`, and `engine`'s own `tests/`
directories — roughly a dozen near-identical `open_pool` functions, seven
copies of `open_file`, and two independent copies of the crash-injection
sweep that is this repository's primary durability correctness gate.

## Architecture

`test-support` sits outside the layered production workspace CLAUDE.md's
dependency-edge rules describe: it is never a normal dependency of any
shipped crate, only a `[dev-dependencies]` entry, added by whichever
crate's `tests/` directory needs it. It depends on `common` and `storage`
only, and only on their public, ungated API - the same constraint any
other crate's own `tests/` file is already under. Because it is
dev-dependency-only, `storage -> test-support -> storage`-shaped edges
(if `test-support` is ever added as `storage`'s own dev-dependency
alongside its self-referential `test-util` entry) do not form a cycle in
the sense the compiler's normal dependency graph cares about; see
CLAUDE.md's "Dependency-edge rules" section for the one paragraph this
crate is the exception to.

## Key Components

- `crash` - the crash-injection sweep algorithm (`CrashWorkload`,
  `assert_workload_is_crash_safe`), generalized from two independent
  copies. See [crash.MD](src/crash.MD).
- `db` - `db_config`, a one-line `common::DbConfig` builder. See
  [db.MD](src/db.MD).
- `devices` - fault-injecting and call-counting `BlockDevice`/
  `SegmentStore` wrappers (`faulty_devices`, `FaultySegmentStore`,
  `CountingDevice`, `CountingSegmentStore`) plus `open_file`. See
  [devices.MD](src/devices.MD).
- `logging` - a thread-local tracing capture rig (`CaptureBuf`,
  `captured_events`, `set_capturing_subscriber`). See
  [logging.MD](src/logging.MD).
- `pool` - `PoolOptions`/`open_pool`/`open_pool_at_path`, one
  parameterised way to stand up a `storage::buffer::BufferPool`. See
  [pool.MD](src/pool.MD).

## Features

Everything here is test scaffolding; there is no production feature to
speak of. What "works" is exactly what it replaces: the same fault
injection, buffer-pool setup, log capture, and crash-sweep behavior that
used to be copy-pasted per crate, now defined once.

## Dependencies

Workspace: `common`, `storage` (with `test-util` enabled, since this
crate's whole purpose is exposing test-only constructors like
`storage::disk::DiskManager::open_with_device` and
`storage::wal::LogManager::open_with_segment_store` to its own callers).
External, dev-shaped but listed as ordinary dependencies since this whole
crate is a testing fixture: `tempfile`, `tracing`, `tracing-subscriber`,
`serde_json`.

## Configuration

None. This crate has no `Cargo.toml` feature gates of its own - it is
already conditional in the only way that matters, by never being a normal
dependency of anything.

## Testing

There is no `tests/` directory here - this crate *is* test infrastructure,
exercised indirectly by every suite that depends on it
(`cargo test -p storage`, `-p catalog`, `-p executor`, `-p txn`,
`-p engine`). A change here that breaks something is caught by whichever
of those suites next runs, which is also why every public item's own
`.MD` documents the specific call sites it replaced - so a regression
there is traceable back to a concrete prior behavior to compare against.
