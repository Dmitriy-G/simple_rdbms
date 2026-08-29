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

Run via `cargo test -p engine --test database_lock`. This is the one test
in this file rather than the three `storage`'s `database_lock.rs` has,
since `DiskManager::open` (via `Database::open_impl`) is the only path
that ever takes the lock in the first place - `Database::open_with_devices`
routes through `DiskManager::open_with_device`, which stays lock-free.
