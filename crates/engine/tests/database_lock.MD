# database_lock

The end-to-end case of `DiskManager::open`'s exclusive advisory lock
(`storage::block_device.MD`'s `lock_exclusive`, exercised directly against
`storage::disk::DiskManager` in `crates/storage/tests/database_lock.rs`):
a second `engine::Database::open` against a database another `Database`
already has open must fail with an error a caller can actually match on
and print, not a bare OS error code.

## Key Components

- `a_second_database_open_reports_a_usable_error` - opens a database,
  then opens the same `db_path` again while the first `Database` is still
  alive. Asserts the second `open` returns `common::Error::DatabaseLocked`
  specifically (not just "some error"), and that its `sql_state()` is
  `SqlState::LOCK_NOT_AVAILABLE` - the two things `cli`/a future server
  would actually use to print "another process has this database open"
  instead of a raw `EWOULDBLOCK`/`ERROR_LOCK_VIOLATION`.
- `SegmentSnapshot` - every currently-existing WAL segment's path paired
  with its full contents, in id order.
- `segment_bytes(wal_base) -> Result<SegmentSnapshot, _>` - reads
  every currently-existing WAL segment file's full contents, sorted by
  segment id (`FileSegmentStore::existing_segments` already returns them
  that way). Used to prove a refused open touched nothing, not just that
  it failed.
- `a_refused_second_open_does_not_touch_the_wal` - the test the earlier
  one doesn't cover: that a refused second open failed *harmlessly*.
  `Database::open_impl` used to open the write-ahead log before taking the
  database's exclusive lock, so a second process could run the log's full
  bootstrap - including `scan_segment`'s tail-trimming `set_len` - against
  a live database before ever discovering it was locked out. Opens a
  first database, commits real writes, then appends deliberate garbage
  past the active segment's last valid record (`open(append)`, bypassing
  the live `Database` entirely) to stand in for bytes a concurrent writer
  has genuinely in flight but not yet complete - exactly what
  `scan_segment`'s trim would discard if it ever ran against this file
  again. Records every segment's exact bytes, attempts and confirms the
  refused second open, then asserts every segment's bytes are still
  byte-identical. This fails on the pre-fix ordering (the appended garbage
  gets silently trimmed away by the second open's own bootstrap) and
  passes once `open_impl` takes the lock - via `DiskManager::open` -
  before ever calling `LogManager::open` (`database.MD`).
- `a_database_can_be_reopened_immediately_after_the_previous_one_drops` -
  opens a `Database` against the same path, `close()`s it, reopens
  immediately, and bare-`drop`s that one, fifty times in a row with no
  work in between. Exercises both teardown shapes `Database` has -
  `close()` then reopen, and a bare `drop` then reopen - because they take
  different paths through teardown and the crash-injection harness only
  ever exercises one of them (`runtime.MD`'s `EngineHandle`/`Drop for
  EngineHandle`). Before `SessionHandle` held the `Arc<EngineHandle>`
  itself, `Database::drop` could return before the engine thread had
  actually released the file's exclusive lock, so the very next `open` on
  the same path could spuriously see `DatabaseLocked`; this test pins
  that a dropped `Database` has fully released the lock by the time
  `drop` returns, not just eventually.

## Usage Example

```rust
use common::{DbConfig, Error};
use engine::Database;

let config = DbConfig::new("locked.db");
let _first = Database::open(config.clone())?;
match Database::open(config) {
    Err(Error::DatabaseLocked { .. }) => {}
    _ => panic!("expected the second open to be refused"),
}
```

Run via `cargo test -p engine --test database_lock`. Two tests here rather
than the three `storage`'s `database_lock.rs` has, since `DiskManager::open`
(via `Database::open_impl`) is the only path that ever takes the lock in
the first place - `Database::open_with_devices` routes through
`DiskManager::open_with_device`, which stays lock-free.
