# pool

One parameterised way to stand up a `storage::buffer::BufferPool` over a
temp-dir (or caller-chosen) database, replacing roughly a dozen
near-identical `open_pool` copies across `storage`, `catalog`, `executor`,
and `txn`'s `tests/` directories that differed only in whether the caller
or the helper owned the temp directory, the LRU-K replacer's `k`, the WAL
segment size, or whether the data file was a real `DiskManager::open` or
a caller-supplied fault-injecting device.

## Key Components

- `PoolOptions` - a builder carrying `pool_size` plus the three axes the
  old copies actually varied on: `replacer_k` (default `2` - only
  `crates/storage/tests/buffer_pool.rs`'s `open_pool_lru` used `1`, to
  exercise the K=1 fallback path deliberately), `segment_size` (`None`
  opens the WAL with `storage::wal::LogManager::open`; `Some(n)` opens it
  with `LogManager::open_with_segment_size(_, n)` - `crates/storage/tests/wal_segments.rs`
  and `wal_truncation_recovery.rs`'s only real divergence from every other
  copy), and `data_device` (`None` opens a real file at the computed data
  path; `Some(device)` opens that device instead while the WAL and DWB
  still go to real files in `dir` - `crates/storage/tests/buffer_pool_flush_races.rs`'s
  `open_pool_with_data_device`). `PoolOptions::new(pool_size)` starts at
  the common defaults; `.replacer_k(k)`/`.segment_size(n)`/`.data_device(d)`
  override one axis each and return `Self` for chaining.
- `open_pool(dir, options) -> Result<BufferPool, _>` - opens `dir/test.db`,
  `dir/test.db.wal`, `dir/test.db.dwb` under the given `options`; the
  shape every copy except `catalog`'s two used.
- `open_pool_at_path(db_path, options) -> Result<BufferPool, _>` - opens
  `db_path` itself as the data file and derives `db_path.wal`/`db_path.dwb`
  by suffix, matching the path convention `engine::EngineHandle::open`
  itself uses in production
  (`crates/engine/src/runtime.MD`) rather than `open_pool`'s
  fixed-filenames-in-a-directory convention; this is what `catalog`'s two
  copies needed and is preserved as its own function rather than folded
  into `open_pool`; the two conventions build genuinely different paths,
  not the same path built two ways.

## Usage Example

```rust
use test_support::{PoolOptions, open_pool};

let dir = tempfile::tempdir()?;
let pool = open_pool(dir.path(), PoolOptions::new(8).replacer_k(1))?;
```

`crates/storage/tests/*.rs`, `crates/catalog/tests/*.rs`,
`crates/executor/tests/*.rs`, and `crates/txn/tests/*.rs` all build their
`BufferPool` this way now; a test that used to own its `tempfile::TempDir`
alongside the pool (returned as a tuple) instead creates the `TempDir`
itself and passes `dir.path()` in, which is one extra line at each such
call site and removes the need for this module to guess whether a caller
wants to keep the directory alive past the call.
