//! Pages written through a `DiskManager`, then the file closed and
//! reopened from scratch, must read back with the same contents.

use std::error::Error;
use std::fs::OpenOptions;
use std::io::{Seek, SeekFrom, Write};

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
fn reopening_with_a_different_page_size_is_a_clear_error() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("test.db");

    {
        let mut disk = DiskManager::open(path.clone(), PAGE_SIZE)?;
        disk.allocate_page()?;
        disk.sync()?;
    }

    let other_page_size = PAGE_SIZE * 2;
    let message = match DiskManager::open(path, other_page_size) {
        Ok(_) => panic!("reopening with a mismatched page size must fail"),
        Err(err) => err.to_string(),
    };
    assert!(
        message.contains(&PAGE_SIZE.to_string()) && message.contains(&other_page_size.to_string()),
        "error should name both the stored and requested page sizes: {message}"
    );

    Ok(())
}

/// A file written under a different (older or newer) on-disk format must be
/// rejected with a clear error, not silently misread - the failure mode
/// this guards against is a real one: `heap::NO_NEXT_PAGE` and the heap
/// page header's layout both changed once already without this check
/// existing yet, and a file written under the old scheme was then
/// misinterpreted deep in the heap/catalog layers as a request for a
/// nonexistent page instead of failing here, at open time, with a
/// diagnosable reason.
#[test]
fn reopening_a_file_with_a_different_format_version_is_a_clear_error() -> Result<(), Box<dyn Error>>
{
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("test.db");

    {
        let mut disk = DiskManager::open(path.clone(), PAGE_SIZE)?;
        disk.allocate_page()?;
        disk.sync()?;
    }

    // Overwrite just the header's version field (bytes 8..12) to simulate a
    // file written by a build with an incompatible on-disk format.
    let mut file = OpenOptions::new().read(true).write(true).open(&path)?;
    file.seek(SeekFrom::Start(8))?;
    file.write_all(&999u32.to_le_bytes())?;
    drop(file);

    let message = match DiskManager::open(path, PAGE_SIZE) {
        Ok(_) => panic!("reopening a file with a stale format version must fail"),
        Err(err) => err.to_string(),
    };
    assert!(
        message.contains("999"),
        "error should name the on-disk file's stored version: {message}"
    );

    Ok(())
}

/// A file whose length isn't a whole multiple of the page size means the
/// last page on disk is partially written - the failure mode this guards
/// against is what would happen if `DiskManager::allocate_page`'s `set_len`
/// were ever interrupted (it currently is not, being a single syscall, but
/// the check must still hold for any other way a file could end up short).
#[test]
fn reopening_a_file_whose_length_is_not_a_page_multiple_is_a_clear_error()
-> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("test.db");

    {
        let mut disk = DiskManager::open(path.clone(), PAGE_SIZE)?;
        disk.allocate_page()?;
        disk.sync()?;
    }

    let truncated_len = (2 * PAGE_SIZE - 10) as u64;
    let file = OpenOptions::new().write(true).open(&path)?;
    file.set_len(truncated_len)?;
    drop(file);

    let message = match DiskManager::open(path, PAGE_SIZE) {
        Ok(_) => panic!("reopening a file whose length isn't a page multiple must fail"),
        Err(err) => err.to_string(),
    };
    assert!(
        message.contains(&truncated_len.to_string()) && message.contains(&PAGE_SIZE.to_string()),
        "error should name both the actual and expected lengths: {message}"
    );

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
