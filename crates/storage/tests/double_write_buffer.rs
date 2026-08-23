//! The double-write buffer, mechanism-level: a batch round-trips through
//! `write_batch`/`read_batch`/`clear_batch`, a corrupted slot copy is
//! skipped rather than trusted, and - the targeted deliverable - a real
//! page torn by a crash mid-flush is restored by
//! `recovery::recover_double_write` from its double-write copy.

use std::cell::Cell;
use std::error::Error;
use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::rc::Rc;

use common::{PageId, TxnId};
use storage::block_device::{BlockDevice, DurabilityModel, FaultyDevice, FileDevice};
use storage::buffer::BufferPool;
use storage::disk::DiskManager;
use storage::dwb::DoubleWriteBuffer;
use storage::page::{PAGE_SIZE, Page};
use storage::recovery;
use storage::replacer::LruKReplacer;
use storage::wal::LogManager;

#[test]
fn write_batch_then_read_batch_round_trips_page_ids_in_order() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let mut dwb = DoubleWriteBuffer::open(
        dir.path().join("test.db.dwb"),
        DoubleWriteBuffer::DEFAULT_CAPACITY,
    )?;

    let mut page_a = Page::new(PageId(1));
    page_a.data_mut()[20..25].copy_from_slice(b"first");
    let mut page_b = Page::new(PageId(2));
    page_b.data_mut()[20..26].copy_from_slice(b"second");

    dwb.write_batch(&[page_a, page_b])?;

    let ids = dwb.read_batch()?.ok_or("expected a batch to be in flight")?;
    assert_eq!(ids, vec![PageId(1), PageId(2)]);

    let slot0 = dwb.read_slot(0)?;
    assert_eq!(&slot0[20..25], b"first");
    let slot1 = dwb.read_slot(1)?;
    assert_eq!(&slot1[20..26], b"second");

    Ok(())
}

#[test]
fn clear_batch_makes_read_batch_report_nothing_to_recover() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let mut dwb = DoubleWriteBuffer::open(
        dir.path().join("test.db.dwb"),
        DoubleWriteBuffer::DEFAULT_CAPACITY,
    )?;

    let page = Page::new(PageId(1));
    dwb.write_batch(&[page])?;
    assert!(dwb.read_batch()?.is_some(), "a batch should be in flight before clearing");

    dwb.clear_batch()?;
    assert_eq!(dwb.read_batch()?, None, "a cleared batch must report nothing to recover");

    Ok(())
}

/// A freshly opened, never-used double-write buffer has no batch in
/// flight: `recover_double_write` must be a clean no-op, leaving the real
/// data file untouched.
#[test]
fn recover_double_write_is_a_no_op_when_nothing_was_in_flight() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let mut disk = DiskManager::open(dir.path().join("test.db"), PAGE_SIZE)?;
    let mut dwb = DoubleWriteBuffer::open(
        dir.path().join("test.db.dwb"),
        DoubleWriteBuffer::DEFAULT_CAPACITY,
    )?;

    let page_id = disk.allocate_page()?;
    let mut original = Page::new(page_id);
    original.data_mut()[20..27].copy_from_slice(b"pristin");
    disk.write_page(page_id, &original)?;
    disk.sync()?;

    recovery::recover_double_write(&mut disk, &mut dwb)?;

    let mut after = Page::new(page_id);
    disk.read_page(page_id, &mut after)?;
    assert_eq!(&after.data()[20..27], b"pristin");
    Ok(())
}

/// A double-write copy corrupted after being written (its own checksum no
/// longer verifies) must be treated as "the crash landed while this copy
/// was still being written" - skipped, never trusted - leaving the real
/// page exactly as it was, not overwritten with garbage.
#[test]
fn a_corrupted_double_write_copy_is_skipped_not_restored_from() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let db_path = dir.path().join("test.db");
    let dwb_path = dir.path().join("test.db.dwb");

    let mut disk = DiskManager::open(&db_path, PAGE_SIZE)?;
    let page_id = disk.allocate_page()?;
    let mut original = Page::new(page_id);
    original.data_mut()[20..28].copy_from_slice(b"original");
    disk.write_page(page_id, &original)?;
    disk.sync()?;

    let mut dwb = DoubleWriteBuffer::open(&dwb_path, DoubleWriteBuffer::DEFAULT_CAPACITY)?;
    let mut would_be_new = Page::new(page_id);
    would_be_new.data_mut()[20..27].copy_from_slice(b"new img");
    dwb.write_batch(&[would_be_new])?;
    drop(dwb);

    // Flip a byte inside slot 0's image (DWB page 1, i.e. file offset
    // `PAGE_SIZE..2*PAGE_SIZE`), corrupting its checksum.
    let mut file = OpenOptions::new().read(true).write(true).open(&dwb_path)?;
    file.seek(SeekFrom::Start(PAGE_SIZE as u64 + 20))?;
    let mut byte = [0u8; 1];
    file.read_exact(&mut byte)?;
    byte[0] ^= 0xFF;
    file.seek(SeekFrom::Start(PAGE_SIZE as u64 + 20))?;
    file.write_all(&byte)?;
    drop(file);

    let mut dwb = DoubleWriteBuffer::open(&dwb_path, DoubleWriteBuffer::DEFAULT_CAPACITY)?;
    recovery::recover_double_write(&mut disk, &mut dwb)?;

    let mut after = Page::new(page_id);
    disk.read_page(page_id, &mut after)?;
    assert_eq!(
        &after.data()[20..28],
        b"original",
        "a corrupted double-write copy must never overwrite the real page"
    );
    Ok(())
}

/// The targeted deliverable: tears the real-file write for one specific
/// dirtied page mid-`flush_all` (via a `FaultyDevice` wrapping only the
/// data file, so the double-write buffer's own writes always land
/// cleanly), then proves `recovery::recover_double_write` - not just
/// "recovery in general" - is what restores it.
#[test]
fn recover_double_write_restores_a_page_torn_mid_flush() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let db_path = dir.path().join("test.db");
    let dwb_path = dir.path().join("test.db.dwb");
    let wal_path = dir.path().join("test.db.wal");

    let page_id;
    {
        let counter = Rc::new(Cell::new(0));
        let db_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&db_path)?;
        // Call 1 is `DiskManager::open_with_device`'s own header write (the
        // device starts zero-length); call 2 is `allocate_page`'s
        // `set_len`; call 3 is the real-file page write inside
        // `flush_all` - the one this test tears.
        let db_device: Box<dyn BlockDevice> = Box::new(FaultyDevice::with_model(
            Box::new(FileDevice::new(db_file)),
            counter.clone(),
            2,
            DurabilityModel::TornWrite,
        ));
        let disk = DiskManager::open_with_device(db_device, PAGE_SIZE, None)?;
        let dwb = DoubleWriteBuffer::open(&dwb_path, DoubleWriteBuffer::DEFAULT_CAPACITY)?;
        let log = LogManager::open(&wal_path)?;
        let pool = BufferPool::new(disk, dwb, log, 4, Box::new(LruKReplacer::new(4, 2)));

        let (pid, mut guard) = pool.new_page(TxnId(0))?;
        page_id = pid;
        // Bytes in both halves of the page, so a first-half-only torn
        // write is guaranteed to produce a checksum mismatch rather than
        // accidentally matching (the untouched second half would
        // otherwise still read as the same zeros it started as).
        guard.write(TxnId(0), 20, b"first half")?;
        guard.write(TxnId(0), 3000, b"second half")?;
        drop(guard);

        let result = pool.flush_all();
        assert!(result.is_err(), "the torn write must surface as an error, simulating a crash");
        // `pool` is dropped here without a clean close, exactly like a
        // real crash.
    }

    // Reopen fresh, on real (non-faulty) devices for the same files - the
    // double-write buffer's own batch (steps 1-3, which completed and
    // synced before the torn real-file write in step 4) is still there.
    let mut disk = DiskManager::open(&db_path, PAGE_SIZE)?;
    let mut dwb = DoubleWriteBuffer::open(&dwb_path, DoubleWriteBuffer::DEFAULT_CAPACITY)?;

    recovery::recover_double_write(&mut disk, &mut dwb)?;

    assert_eq!(
        dwb.read_batch()?,
        None,
        "recover_double_write must retire the batch once it has been acted on"
    );

    let mut restored = Page::new(page_id);
    disk.read_page(page_id, &mut restored)?;
    assert_eq!(&restored.data()[20..30], b"first half");
    assert_eq!(&restored.data()[3000..3011], b"second half");
    Ok(())
}
