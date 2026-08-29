# database_lock

`DiskManager::open`'s exclusive advisory lock (`block_device.MD`'s
`lock_exclusive`): a second process, or a second `DiskManager::open` call
in this same process, must never be able to open the same database file
while the first is still using it, and the lock must not outlive the
`DiskManager` that took it.

## Key Components

- `a_second_open_of_the_same_file_is_refused` - opens a fresh database
  file, then opens the same path again while the first `DiskManager` is
  still alive. The second call must fail with
  `StorageError::DatabaseLocked`, not any other error and not success.
- `the_lock_is_released_when_the_manager_is_dropped` - opens, drops the
  `DiskManager`, then opens the same path again. The second open must
  succeed, proving the lock is tied to the `DiskManager`'s (and the
  `File`'s) lifetime rather than leaking past it.
- `open_with_device_takes_no_lock_and_never_conflicts` - opens the same
  underlying file twice through `DiskManager::open_with_device` (each
  wrapped in its own `FileDevice` over an independently opened `File`),
  proving this path never calls `lock_exclusive` at all - the crash and
  in-memory device tests elsewhere in this crate construct devices this
  way and must not need a real, lockable file to do it.

## Usage Example

```rust
use storage::StorageError;
use storage::disk::DiskManager;
use storage::page::PAGE_SIZE;

let _first = DiskManager::open("locked.db", PAGE_SIZE)?;
match DiskManager::open("locked.db", PAGE_SIZE) {
    Err(StorageError::DatabaseLocked { .. }) => {}
    _ => panic!("expected the second open to be refused"),
}
```

Run via `cargo test -p storage --test database_lock`. The end-to-end case -
that `engine::Database::open` surfaces this as a `common::Error` a caller
can actually match on and print - lives in
`crates/engine/tests/database_lock.rs` instead, since `storage` cannot
depend on `engine` (`docs/adr/0002-crate-splitting.md`'s dependency-edge
rules) and this crate's own tests can only reach `storage::disk::DiskManager`
directly.
