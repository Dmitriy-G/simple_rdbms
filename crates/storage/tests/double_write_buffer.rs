use std::error::Error;
use std::fs::OpenOptions;
use std::io;
use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use common::crc::crc32;
use common::{PageId, TxnId};
use storage::StorageError;
use storage::block_device::{BlockDevice, DurabilityModel, FaultyDevice, FileDevice};
use storage::buffer::BufferPool;
use storage::disk::DiskManager;
use storage::dwb::DoubleWriteBuffer;
use storage::page::{PAGE_SIZE, Page};
use storage::recovery;
use storage::replacer::LruKReplacer;
use storage::wal::LogManager;
use test_support::open_file;

fn read_raw_page(
    db_path: &std::path::Path,
    page_id: PageId,
) -> Result<[u8; PAGE_SIZE], Box<dyn Error>> {
    let mut buf = [0u8; PAGE_SIZE];
    let mut file = open_file(db_path)?;
    file.seek(SeekFrom::Start(page_id.0 as u64 * PAGE_SIZE as u64))?;
    file.read_exact(&mut buf)?;
    Ok(buf)
}

fn tear_page(
    db_path: &std::path::Path,
    disk: &DiskManager,
    page_id: PageId,
    bytes: &[u8],
) -> Result<(), Box<dyn Error>> {
    let mut original = Page::new(page_id);
    original.data_mut()[20..20 + bytes.len()].copy_from_slice(bytes);
    disk.write_page(page_id, &original)?;
    disk.sync()?;

    let offset = page_id.0 as u64 * PAGE_SIZE as u64 + 20;
    let mut file = open_file(db_path)?;
    file.seek(SeekFrom::Start(offset))?;
    let mut byte = [0u8; 1];
    file.read_exact(&mut byte)?;
    byte[0] ^= 0xFF;
    file.seek(SeekFrom::Start(offset))?;
    file.write_all(&byte)?;
    Ok(())
}

#[test]
fn write_batch_then_read_batch_round_trips_page_ids_in_order() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let dwb = DoubleWriteBuffer::open(
        dir.path().join("test.db.dwb"),
        DoubleWriteBuffer::DEFAULT_CAPACITY,
    )?;

    let mut page_a = Page::new(PageId(1));
    page_a.data_mut()[20..25].copy_from_slice(b"first");
    let mut page_b = Page::new(PageId(2));
    page_b.data_mut()[20..26].copy_from_slice(b"second");

    dwb.write_batch(&[page_a, page_b])?;

    let entries = dwb.read_batch()?.ok_or("expected a batch to be in flight")?;
    let ids: Vec<PageId> = entries.iter().map(|&(id, _)| id).collect();
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
    let dwb = DoubleWriteBuffer::open(
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

#[test]
fn recover_double_write_is_a_no_op_when_nothing_was_in_flight() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let disk = DiskManager::open(dir.path().join("test.db"), PAGE_SIZE)?;
    let dwb = DoubleWriteBuffer::open(
        dir.path().join("test.db.dwb"),
        DoubleWriteBuffer::DEFAULT_CAPACITY,
    )?;

    let page_id = disk.allocate_page()?;
    let mut original = Page::new(page_id);
    original.data_mut()[20..27].copy_from_slice(b"pristin");
    disk.write_page(page_id, &original)?;
    disk.sync()?;

    recovery::recover_double_write(&disk, &dwb)?;

    let mut after = Page::new(page_id);
    disk.read_page(page_id, &mut after)?;
    assert_eq!(&after.data()[20..27], b"pristin");
    Ok(())
}

#[test]
fn a_corrupted_double_write_copy_is_skipped_not_restored_from() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let db_path = dir.path().join("test.db");
    let dwb_path = dir.path().join("test.db.dwb");

    let disk = DiskManager::open(&db_path, PAGE_SIZE)?;
    let page_id = disk.allocate_page()?;
    let mut original = Page::new(page_id);
    original.data_mut()[20..28].copy_from_slice(b"original");
    disk.write_page(page_id, &original)?;
    disk.sync()?;

    let dwb = DoubleWriteBuffer::open(&dwb_path, DoubleWriteBuffer::DEFAULT_CAPACITY)?;
    let mut would_be_new = Page::new(page_id);
    would_be_new.data_mut()[20..27].copy_from_slice(b"new img");
    dwb.write_batch(&[would_be_new])?;
    drop(dwb);

    let mut file = OpenOptions::new().read(true).write(true).open(&dwb_path)?;
    file.seek(SeekFrom::Start(PAGE_SIZE as u64 + 20))?;
    let mut byte = [0u8; 1];
    file.read_exact(&mut byte)?;
    byte[0] ^= 0xFF;
    file.seek(SeekFrom::Start(PAGE_SIZE as u64 + 20))?;
    file.write_all(&byte)?;
    drop(file);

    let dwb = DoubleWriteBuffer::open(&dwb_path, DoubleWriteBuffer::DEFAULT_CAPACITY)?;
    recovery::recover_double_write(&disk, &dwb)?;

    let mut after = Page::new(page_id);
    disk.read_page(page_id, &mut after)?;
    assert_eq!(
        &after.data()[20..28],
        b"original",
        "a corrupted double-write copy must never overwrite the real page"
    );
    Ok(())
}

#[test]
fn recover_double_write_restores_a_page_torn_mid_flush() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let db_path = dir.path().join("test.db");
    let dwb_path = dir.path().join("test.db.dwb");
    let wal_path = dir.path().join("test.db.wal");

    let page_id;
    {
        let counter = Arc::new(AtomicU64::new(0));
        let db_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&db_path)?;
        let db_device: Box<dyn BlockDevice> = Box::new(FaultyDevice::with_torn_sectors(
            Box::new(FileDevice::new(db_file)),
            counter.clone(),
            2,
            DurabilityModel::torn_write(),
            vec![0, 1, 2, 3],
        ));
        let disk = DiskManager::open_with_device(db_device, PAGE_SIZE, None)?;
        let dwb = DoubleWriteBuffer::open(&dwb_path, DoubleWriteBuffer::DEFAULT_CAPACITY)?;
        let log = LogManager::open(&wal_path)?;
        let pool = BufferPool::new(disk, dwb, log, 4, Box::new(LruKReplacer::new(4, 2)));

        let (pid, mut guard) = pool.new_page(TxnId(0))?;
        page_id = pid;
        guard.write(TxnId(0), 20, b"first half")?;
        guard.write(TxnId(0), 3000, b"second half")?;
        drop(guard);

        let result = pool.flush_all();
        assert!(result.is_err(), "the torn write must surface as an error, simulating a crash");
    }

    let disk = DiskManager::open(&db_path, PAGE_SIZE)?;
    let dwb = DoubleWriteBuffer::open(&dwb_path, DoubleWriteBuffer::DEFAULT_CAPACITY)?;

    let mut precheck = Page::new(page_id);
    assert!(
        disk.read_page(page_id, &mut precheck).is_err(),
        "the torn write must leave the real page failing its own checksum before recovery runs"
    );

    recovery::recover_double_write(&disk, &dwb)?;

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

fn assert_restores_a_page_torn_at_sectors(sectors: Vec<usize>) -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let db_path = dir.path().join("test.db");
    let dwb_path = dir.path().join("test.db.dwb");
    let wal_path = dir.path().join("test.db.wal");

    let page_id;
    {
        let counter = Arc::new(AtomicU64::new(0));
        let db_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&db_path)?;
        let db_device: Box<dyn BlockDevice> = Box::new(FaultyDevice::with_torn_sectors(
            Box::new(FileDevice::new(db_file)),
            counter.clone(),
            2,
            DurabilityModel::torn_write(),
            sectors,
        ));
        let disk = DiskManager::open_with_device(db_device, PAGE_SIZE, None)?;
        let dwb = DoubleWriteBuffer::open(&dwb_path, DoubleWriteBuffer::DEFAULT_CAPACITY)?;
        let log = LogManager::open(&wal_path)?;
        let pool = BufferPool::new(disk, dwb, log, 4, Box::new(LruKReplacer::new(4, 2)));

        let (pid, mut guard) = pool.new_page(TxnId(0))?;
        page_id = pid;
        guard.write(TxnId(0), 20, b"first half")?;
        guard.write(TxnId(0), 3600, b"last sect.")?;
        drop(guard);

        let result = pool.flush_all();
        assert!(result.is_err(), "the torn write must surface as an error, simulating a crash");
    }

    let disk = DiskManager::open(&db_path, PAGE_SIZE)?;
    let dwb = DoubleWriteBuffer::open(&dwb_path, DoubleWriteBuffer::DEFAULT_CAPACITY)?;

    let mut precheck = Page::new(page_id);
    assert!(
        disk.read_page(page_id, &mut precheck).is_err(),
        "the torn write must leave the real page failing its own checksum before recovery runs"
    );

    recovery::recover_double_write(&disk, &dwb)?;

    assert_eq!(
        dwb.read_batch()?,
        None,
        "recover_double_write must retire the batch once it has been acted on"
    );

    let mut restored = Page::new(page_id);
    disk.read_page(page_id, &mut restored)?;
    assert_eq!(&restored.data()[20..30], b"first half");
    assert_eq!(&restored.data()[3600..3610], b"last sect.");
    Ok(())
}

#[test]
fn recover_double_write_restores_a_page_torn_at_only_its_first_sector() -> Result<(), Box<dyn Error>>
{
    assert_restores_a_page_torn_at_sectors(vec![0])
}

#[test]
fn recover_double_write_restores_a_page_torn_at_only_its_last_sector() -> Result<(), Box<dyn Error>>
{
    assert_restores_a_page_torn_at_sectors(vec![PAGE_SIZE / 512 - 1])
}

#[test]
fn recover_double_write_leaves_the_batch_in_place_when_its_own_restore_write_tears()
-> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let db_path = dir.path().join("test.db");
    let dwb_path = dir.path().join("test.db.dwb");
    let wal_path = dir.path().join("test.db.wal");

    let page_id;
    {
        let counter = Arc::new(AtomicU64::new(0));
        let db_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&db_path)?;
        let db_device: Box<dyn BlockDevice> = Box::new(FaultyDevice::with_torn_sectors(
            Box::new(FileDevice::new(db_file)),
            counter.clone(),
            2,
            DurabilityModel::torn_write(),
            vec![0],
        ));
        let disk = DiskManager::open_with_device(db_device, PAGE_SIZE, None)?;
        let dwb = DoubleWriteBuffer::open(&dwb_path, DoubleWriteBuffer::DEFAULT_CAPACITY)?;
        let log = LogManager::open(&wal_path)?;
        let pool = BufferPool::new(disk, dwb, log, 4, Box::new(LruKReplacer::new(4, 2)));

        let (pid, mut guard) = pool.new_page(TxnId(0))?;
        page_id = pid;
        guard.write(TxnId(0), 20, b"first half")?;
        guard.write(TxnId(0), 3000, b"second half")?;
        drop(guard);

        let result = pool.flush_all();
        assert!(result.is_err(), "the torn write must surface as an error, simulating a crash");
    }

    {
        let counter = Arc::new(AtomicU64::new(0));
        let db_file = OpenOptions::new().read(true).write(true).open(&db_path)?;
        let db_device: Box<dyn BlockDevice> = Box::new(FaultyDevice::with_torn_sectors(
            Box::new(FileDevice::new(db_file)),
            counter.clone(),
            0,
            DurabilityModel::torn_write(),
            vec![4],
        ));
        let disk = DiskManager::open_with_device(db_device, PAGE_SIZE, None)?;
        let dwb = DoubleWriteBuffer::open(&dwb_path, DoubleWriteBuffer::DEFAULT_CAPACITY)?;

        let result = recovery::recover_double_write(&disk, &dwb);
        assert!(
            result.is_err(),
            "a restore write torn by a second crash must itself surface as an error"
        );
    }

    let dwb_check = DoubleWriteBuffer::open(&dwb_path, DoubleWriteBuffer::DEFAULT_CAPACITY)?;
    assert!(
        dwb_check.read_batch()?.is_some(),
        "a failed restore must leave the batch in place instead of clearing it"
    );
    drop(dwb_check);

    let disk = DiskManager::open(&db_path, PAGE_SIZE)?;
    let dwb = DoubleWriteBuffer::open(&dwb_path, DoubleWriteBuffer::DEFAULT_CAPACITY)?;
    recovery::recover_double_write(&disk, &dwb)?;
    assert_eq!(dwb.read_batch()?, None, "a clean retry must retire the batch");

    let mut restored = Page::new(page_id);
    disk.read_page(page_id, &mut restored)?;
    assert_eq!(&restored.data()[20..30], b"first half");
    assert_eq!(&restored.data()[3000..3011], b"second half");
    Ok(())
}

struct SilentlyDropsOneWrite {
    inner: Box<dyn BlockDevice>,
    calls: AtomicUsize,
    drop_at: usize,
}

impl BlockDevice for SilentlyDropsOneWrite {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        self.inner.read_at(offset, buf)
    }

    fn write_at(&self, offset: u64, buf: &[u8]) -> io::Result<()> {
        let calls = self.calls.fetch_add(1, Ordering::Relaxed) + 1;
        if calls == self.drop_at { Ok(()) } else { self.inner.write_at(offset, buf) }
    }

    fn set_len(&self, len: u64) -> io::Result<()> {
        self.inner.set_len(len)
    }

    fn sync_all(&self) -> io::Result<()> {
        self.inner.sync_all()
    }

    fn size(&self) -> io::Result<u64> {
        self.inner.size()
    }
}

#[test]
fn recover_double_write_refuses_to_clear_the_batch_when_a_restore_write_silently_fails()
-> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let db_path = dir.path().join("test.db");
    let dwb_path = dir.path().join("test.db.dwb");

    let disk = DiskManager::open(&db_path, PAGE_SIZE)?;
    let page_id = disk.allocate_page()?;
    let mut original = Page::new(page_id);
    original.data_mut()[20..28].copy_from_slice(b"original");
    disk.write_page(page_id, &original)?;
    disk.sync()?;
    drop(disk);

    let dwb = DoubleWriteBuffer::open(&dwb_path, DoubleWriteBuffer::DEFAULT_CAPACITY)?;
    let mut good_copy = Page::new(page_id);
    good_copy.data_mut()[20..28].copy_from_slice(b"original");
    dwb.write_batch(&[good_copy])?;
    drop(dwb);

    let corrupt_at = page_id.0 as u64 * PAGE_SIZE as u64 + 20;
    let mut file = OpenOptions::new().read(true).write(true).open(&db_path)?;
    file.seek(SeekFrom::Start(corrupt_at))?;
    let mut byte = [0u8; 1];
    file.read_exact(&mut byte)?;
    byte[0] ^= 0xFF;
    file.seek(SeekFrom::Start(corrupt_at))?;
    file.write_all(&byte)?;
    drop(file);

    let db_file = OpenOptions::new().read(true).write(true).open(&db_path)?;
    let db_device: Box<dyn BlockDevice> = Box::new(SilentlyDropsOneWrite {
        inner: Box::new(FileDevice::new(db_file)),
        calls: AtomicUsize::new(0),
        drop_at: 1,
    });
    let disk = DiskManager::open_with_device(db_device, PAGE_SIZE, None)?;
    let dwb = DoubleWriteBuffer::open(&dwb_path, DoubleWriteBuffer::DEFAULT_CAPACITY)?;

    let result = recovery::recover_double_write(&disk, &dwb);
    assert!(
        matches!(
            result,
            Err(StorageError::DoubleWriteRestoreFailed { page_id: pid }) if pid == page_id.0
        ),
        "a restore write that silently doesn't land must be caught before the batch is cleared, \
         got {result:?}"
    );

    let dwb_check = DoubleWriteBuffer::open(&dwb_path, DoubleWriteBuffer::DEFAULT_CAPACITY)?;
    assert!(
        dwb_check.read_batch()?.is_some(),
        "a refused clear must leave the batch in place instead of clearing it"
    );
    Ok(())
}

#[test]
fn open_rejects_zero_capacity() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let result = DoubleWriteBuffer::open(dir.path().join("test.db.dwb"), 0);
    assert!(
        matches!(result, Err(StorageError::InvalidDwbCapacity)),
        "capacity 0 must be rejected with InvalidDwbCapacity"
    );
    Ok(())
}

#[test]
fn a_slot_whose_crc_does_not_match_is_not_restored() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let db_path = dir.path().join("test.db");
    let dwb_path = dir.path().join("test.db.dwb");

    let disk = DiskManager::open_with_device(
        Box::new(FileDevice::new(open_file(&db_path)?)),
        PAGE_SIZE,
        None,
    )?;
    let page_id = disk.allocate_page()?;
    tear_page(&db_path, &disk, page_id, b"original")?;

    let dwb = DoubleWriteBuffer::open(&dwb_path, DoubleWriteBuffer::DEFAULT_CAPACITY)?;
    let slot_content = [0xABu8; PAGE_SIZE];
    dwb.write_raw_slot(0, &slot_content)?;
    let wrong_crc = crc32(&slot_content).wrapping_add(1);
    dwb.write_raw_header_entries(&[(page_id, wrong_crc)])?;

    let before = read_raw_page(&db_path, page_id)?;

    recovery::recover_double_write(&disk, &dwb)?;

    let after = read_raw_page(&db_path, page_id)?;
    assert_eq!(
        after, before,
        "a slot whose recorded crc does not match its content must never be restored from"
    );
    Ok(())
}

#[test]
fn an_all_zero_slot_is_never_written_over_a_live_page() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let db_path = dir.path().join("test.db");
    let dwb_path = dir.path().join("test.db.dwb");

    let disk = DiskManager::open_with_device(
        Box::new(FileDevice::new(open_file(&db_path)?)),
        PAGE_SIZE,
        None,
    )?;
    let page_id = disk.allocate_page()?;
    tear_page(&db_path, &disk, page_id, b"original")?;

    let dwb = DoubleWriteBuffer::open(&dwb_path, DoubleWriteBuffer::DEFAULT_CAPACITY)?;
    let zero_slot = [0u8; PAGE_SIZE];
    dwb.write_raw_slot(0, &zero_slot)?;
    dwb.write_raw_header_entries(&[(page_id, crc32(&zero_slot))])?;

    let before = read_raw_page(&db_path, page_id)?;

    recovery::recover_double_write(&disk, &dwb)?;

    let after = read_raw_page(&db_path, page_id)?;
    assert_eq!(
        after, before,
        "an all-zero slot must never be written over a live page, even when its recorded crc \
         matches"
    );
    Ok(())
}

#[test]
fn a_slot_holding_the_wrong_page_id_fails_recovery_loudly() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let db_path = dir.path().join("test.db");
    let dwb_path = dir.path().join("test.db.dwb");

    let disk = DiskManager::open_with_device(
        Box::new(FileDevice::new(open_file(&db_path)?)),
        PAGE_SIZE,
        None,
    )?;
    let page_a = disk.allocate_page()?;
    let page_b = disk.allocate_page()?;
    tear_page(&db_path, &disk, page_a, b"page-a-x")?;

    let dwb = DoubleWriteBuffer::open(&dwb_path, DoubleWriteBuffer::DEFAULT_CAPACITY)?;
    let mut content_a = Page::new(page_a);
    content_a.data_mut()[20..28].copy_from_slice(b"a-backup");
    let mut content_b = Page::new(page_b);
    content_b.data_mut()[20..28].copy_from_slice(b"b-backup");
    dwb.write_batch(&[content_a, content_b])?;

    let slot1 = dwb.read_slot(1)?;
    dwb.write_raw_slot(0, &slot1)?;

    let before = read_raw_page(&db_path, page_a)?;

    let result = recovery::recover_double_write(&disk, &dwb);
    match &result {
        Err(StorageError::DoubleWriteRestoreFailed { page_id }) if *page_id == page_a.0 => {}
        other => panic!(
            "expected DoubleWriteRestoreFailed naming page {page_a:?}, got a different result: \
             {other:?}"
        ),
    }

    let after = read_raw_page(&db_path, page_a)?;
    assert_eq!(
        after, before,
        "a slot holding a different page's content than its header claims must never be \
         restored from"
    );
    Ok(())
}
