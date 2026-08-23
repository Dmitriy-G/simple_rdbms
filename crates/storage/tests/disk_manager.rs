//! Pages written through a `DiskManager`, then the file closed and
//! reopened from scratch, must read back with the same contents.

use std::error::Error;
use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use common::PageId;
use storage::StorageError;
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

        // Offset 20, past both the checksum (`0..4`) and `page_lsn`
        // (`4..12`) prefixes `DiskManager::write_page` reserves and
        // stamps/reads itself - writing into either would either be
        // clobbered by the checksum it computes or misread as a bogus LSN.
        let mut page1 = Page::new(id1);
        page1.data_mut()[20..25].copy_from_slice(b"hello");
        disk.write_page(id1, &page1)?;

        let mut page2 = Page::new(id2);
        page2.data_mut()[20..25].copy_from_slice(b"world");
        disk.write_page(id2, &page2)?;

        disk.sync()?;
        (id1, id2)
    };

    let mut disk = DiskManager::open(path, PAGE_SIZE)?;

    let mut page1 = Page::new(id1);
    disk.read_page(id1, &mut page1)?;
    assert_eq!(&page1.data()[20..25], b"hello");

    let mut page2 = Page::new(id2);
    disk.read_page(id2, &mut page2)?;
    assert_eq!(&page2.data()[20..25], b"world");

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

    // Overwrite just the header's version field (bytes 20..24 - after page
    // 0's own 4-byte checksum, 8-byte `page_lsn` prefix, and the 8-byte
    // magic) to simulate a file written by a build with an incompatible
    // on-disk format. `DiskManager::open` reads this raw, before any
    // checksum verification, so it fails on the version check rather than
    // `ChecksumMismatch`.
    let mut file = OpenOptions::new().read(true).write(true).open(&path)?;
    file.seek(SeekFrom::Start(20))?;
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

/// Flips every bit of the byte at `offset_in_page` within `page_id`'s slot
/// in the database file at `path`, simulating single-byte disk corruption
/// (media bit rot, a stray write from outside the engine) that happened
/// after the page was durably written.
fn flip_byte_at(path: &Path, page_id: PageId, offset_in_page: u64) -> Result<(), Box<dyn Error>> {
    let file_offset = page_id.0 as u64 * PAGE_SIZE as u64 + offset_in_page;
    let mut file = OpenOptions::new().read(true).write(true).open(path)?;
    file.seek(SeekFrom::Start(file_offset))?;
    let mut byte = [0u8; 1];
    file.read_exact(&mut byte)?;
    byte[0] ^= 0xFF;
    file.seek(SeekFrom::Start(file_offset))?;
    file.write_all(&byte)?;
    Ok(())
}

/// A page allocated (extending the file) but never written back through
/// `write_page` reads back as a clean, all-zero page rather than tripping
/// checksum verification. This is the exception `read_page` documents for
/// the `NO_NEXT_PAGE` sentinel: a CRC-32 over 4092 zero bytes is not zero,
/// so without this exception every freshly-allocated page would fail
/// verification the first time anything read it back.
#[test]
fn freshly_allocated_never_written_page_reads_back_clean() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("test.db");
    let mut disk = DiskManager::open(path, PAGE_SIZE)?;

    let page_id = disk.allocate_page()?;
    let mut page = Page::new(page_id);
    disk.read_page(page_id, &mut page)?;
    assert!(page.data().iter().all(|&b| b == 0), "a never-written page should read back all zero");
    Ok(())
}

/// A byte flipped in what would be a slotted page's header region (just
/// past the checksum and `page_lsn` prefix) is caught the next time the
/// page is read from disk, naming the offending page id.
#[test]
fn flipped_byte_in_the_header_region_is_a_checksum_mismatch() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("test.db");

    let page_id = {
        let mut disk = DiskManager::open(path.clone(), PAGE_SIZE)?;
        let page_id = disk.allocate_page()?;
        let mut page = Page::new(page_id);
        // Non-zero content, so this isn't the all-zero page that skips
        // verification.
        page.data_mut()[12..14].copy_from_slice(&3u16.to_le_bytes());
        disk.write_page(page_id, &page)?;
        disk.sync()?;
        page_id
    };

    flip_byte_at(&path, page_id, 12)?;

    let mut disk = DiskManager::open(path, PAGE_SIZE)?;
    let mut page = Page::new(page_id);
    match disk.read_page(page_id, &mut page) {
        Err(StorageError::ChecksumMismatch { page_id: reported, .. }) => {
            assert_eq!(reported, page_id.0)
        }
        Err(other) => panic!("expected ChecksumMismatch, got a different error: {other}"),
        Ok(_) => panic!("a flipped header byte must be detected as checksum corruption"),
    }
    Ok(())
}

/// A byte flipped near the end of the page (where tuple payload bytes are
/// packed, in the slotted layout) is caught the same way as a header byte -
/// the checksum covers the whole page, not just its structured fields.
#[test]
fn flipped_byte_in_the_tuple_payload_region_is_a_checksum_mismatch() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("test.db");

    let page_id = {
        let mut disk = DiskManager::open(path.clone(), PAGE_SIZE)?;
        let page_id = disk.allocate_page()?;
        let mut page = Page::new(page_id);
        page.data_mut()[PAGE_SIZE - 1] = 0xAB;
        disk.write_page(page_id, &page)?;
        disk.sync()?;
        page_id
    };

    flip_byte_at(&path, page_id, (PAGE_SIZE - 1) as u64)?;

    let mut disk = DiskManager::open(path, PAGE_SIZE)?;
    let mut page = Page::new(page_id);
    match disk.read_page(page_id, &mut page) {
        Err(StorageError::ChecksumMismatch { page_id: reported, .. }) => {
            assert_eq!(reported, page_id.0)
        }
        Err(other) => panic!("expected ChecksumMismatch, got a different error: {other}"),
        Ok(_) => panic!("a flipped payload byte must be detected as checksum corruption"),
    }
    Ok(())
}

/// A byte flipped inside the checksum field itself (bytes `0..4`) is
/// detected too: the stored checksum no longer matches the value
/// recomputed over the rest of the page.
#[test]
fn flipped_byte_inside_the_checksum_field_is_detected() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("test.db");

    let page_id = {
        let mut disk = DiskManager::open(path.clone(), PAGE_SIZE)?;
        let page_id = disk.allocate_page()?;
        let mut page = Page::new(page_id);
        page.data_mut()[20..25].copy_from_slice(b"hello");
        disk.write_page(page_id, &page)?;
        disk.sync()?;
        page_id
    };

    flip_byte_at(&path, page_id, 1)?;

    let mut disk = DiskManager::open(path, PAGE_SIZE)?;
    let mut page = Page::new(page_id);
    match disk.read_page(page_id, &mut page) {
        Err(StorageError::ChecksumMismatch { page_id: reported, .. }) => {
            assert_eq!(reported, page_id.0)
        }
        Err(other) => panic!("expected ChecksumMismatch, got a different error: {other}"),
        Ok(_) => panic!("a flipped checksum byte must be detected as corruption"),
    }
    Ok(())
}
