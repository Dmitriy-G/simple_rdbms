# config

Top-level configuration for opening a database: where its files live on
disk and the sizing knobs for the storage engine. Constructed by the
`engine` facade (or the `cli`) and threaded down into the disk manager and
buffer pool — every other subsystem receives its sizing knobs already
resolved rather than reaching back up to ask for them.

## Key Components

- `DbConfig` - the configuration struct. `db_path` is the main database
  file's path. `page_size` is fixed for the lifetime of a database file
  and applies to every page, including the WAL's. `buffer_pool_size` is
  the number of frames the buffer pool holds in memory.
  `checkpoint_byte_threshold` is how many bytes may be appended to the
  write-ahead log since the last checkpoint before a new one is triggered,
  bounding how much of the log a future recovery's Analysis pass has to
  scan. `dwb_capacity` is the number of page-image slots the double-write
  buffer's `<db_path>.dwb` file holds, capping how many dirty pages
  `BufferPool::flush_all` can cover in a single batch before it must split
  into more than one. `slow_query_warn_threshold_ms` is how long a
  statement may run before `Database::execute` logs it at `warn` instead
  of the ordinary `debug` per-statement line - see CLAUDE.md's logging
  section.
- `DbConfig::DEFAULT_PAGE_SIZE` - 4 KiB, matching common OS page sizes.
- `DbConfig::DEFAULT_BUFFER_POOL_SIZE` - 64 frames.
- `DbConfig::DEFAULT_CHECKPOINT_BYTE_THRESHOLD` - 4 MiB of log growth.
- `DbConfig::DEFAULT_DWB_CAPACITY` - 64 page-image slots.
- `DbConfig::DEFAULT_SLOW_QUERY_WARN_THRESHOLD_MS` - 100ms.
- `DbConfig::new(db_path) -> Self` - builds a config pointing at `db_path`
  with every other field left at its default.

## Usage Example

```rust
use common::DbConfig;

let config = DbConfig::new("my_database.db");
assert_eq!(config.page_size, DbConfig::DEFAULT_PAGE_SIZE);
```

`engine::Database::open` is the only caller that constructs a `DbConfig` in
the ordinary path (the `cli` builds one from its parsed arguments and hands
it straight to `Database::open`); everything below `engine` just receives
the fields it needs already resolved.
