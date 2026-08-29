use std::error::Error;

use storage::StorageError;
use storage::disk::DiskManager;
use storage::page::PAGE_SIZE;

#[test]
fn a_second_open_of_the_same_file_is_refused() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("locked.db");
    let _first = DiskManager::open(&path, PAGE_SIZE)?;
    match DiskManager::open(&path, PAGE_SIZE) {
        Err(StorageError::DatabaseLocked { .. }) => Ok(()),
        Err(other) => panic!("expected DatabaseLocked, got {other}"),
        Ok(_) => panic!("a second open of a locked database must be refused"),
    }
}

#[test]
fn the_lock_is_released_when_the_manager_is_dropped() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("locked.db");
    drop(DiskManager::open(&path, PAGE_SIZE)?);
    let _reopened = DiskManager::open(&path, PAGE_SIZE)?;
    Ok(())
}

#[test]
fn open_with_device_takes_no_lock_and_never_conflicts() -> Result<(), Box<dyn Error>> {
    use storage::block_device::FileDevice;

    let dir = tempfile::tempdir()?;
    let path = dir.path().join("locked.db");
    let first_file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)?;
    let _first =
        DiskManager::open_with_device(Box::new(FileDevice::new(first_file)), PAGE_SIZE, None)?;

    let second_file = std::fs::OpenOptions::new().read(true).write(true).open(&path)?;
    let _second =
        DiskManager::open_with_device(Box::new(FileDevice::new(second_file)), PAGE_SIZE, None)?;
    Ok(())
}
