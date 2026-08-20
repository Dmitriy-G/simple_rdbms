//! Pages written through a `DiskManager`, then the file closed and
//! reopened from scratch, must read back with the same contents.

use std::error::Error;

use common::PageId;
use storage::disk::DiskManager;
use storage::page::{PAGE_SIZE, Page};

#[test]
fn pages_survive_close_and_reopen() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("test.db");

    let (id1, id2) = {
        let mut disk = DiskManager::open(path.clone(), PAGE_SIZE)?;
        let id1 = disk.allocate_page()?;
        let id2 = disk.allocate_page()?;

        let mut page1 = Page::new(id1);
        page1.data_mut()[0..5].copy_from_slice(b"hello");
        disk.write_page(id1, &page1)?;

        let mut page2 = Page::new(id2);
        page2.data_mut()[0..5].copy_from_slice(b"world");
        disk.write_page(id2, &page2)?;

        disk.sync()?;
        (id1, id2)
    };

    let mut disk = DiskManager::open(path, PAGE_SIZE)?;

    let mut page1 = Page::new(id1);
    disk.read_page(id1, &mut page1)?;
    assert_eq!(&page1.data()[0..5], b"hello");

    let mut page2 = Page::new(id2);
    disk.read_page(id2, &mut page2)?;
    assert_eq!(&page2.data()[0..5], b"world");

    Ok(())
}

#[test]
fn reading_an_unallocated_page_is_an_error_not_zeros() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("test.db");
    let mut disk = DiskManager::open(path, PAGE_SIZE)?;

    let mut page = Page::new(PageId(99));
    let result = disk.read_page(PageId(99), &mut page);
    assert!(result.is_err());

    Ok(())
}
