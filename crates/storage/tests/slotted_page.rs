use std::error::Error;

use common::TxnId;
use storage::buffer::BufferPool;
use storage::disk::DiskManager;
use storage::dwb::DoubleWriteBuffer;
use storage::heap::SlottedPage;
use storage::page::PAGE_SIZE;
use storage::replacer::LruKReplacer;
use storage::wal::LogManager;

const TXN: TxnId = TxnId(0);

fn open_pool() -> Result<(BufferPool, tempfile::TempDir), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let disk = DiskManager::open(dir.path().join("test.db"), PAGE_SIZE)?;
    let dwb = DoubleWriteBuffer::open(
        dir.path().join("test.db.dwb"),
        DoubleWriteBuffer::DEFAULT_CAPACITY,
    )?;
    let log = LogManager::open(dir.path().join("test.db.wal"))?;
    let replacer = Box::new(LruKReplacer::new(4, 2));
    Ok((BufferPool::new(disk, dwb, log, 4, replacer), dir))
}

#[test]
fn insert_returns_none_once_full_and_prior_tuples_stay_readable() -> Result<(), Box<dyn Error>> {
    let (pool, _dir) = open_pool()?;
    let (_page_id, mut guard) = pool.new_page(TXN)?;
    let mut slotted = SlottedPage::new(&mut guard, TXN);
    slotted.init()?;

    let payload = [0xABu8; 200];
    let mut slots = Vec::new();
    while let Some(slot) = slotted.insert(&payload)? {
        slots.push(slot);
    }

    assert!(!slots.is_empty(), "at least one 200-byte tuple should fit in a 4KiB page");

    for slot in slots {
        assert_eq!(slotted.read(slot)?, Some(payload.as_slice()));
    }
    Ok(())
}

#[test]
fn deleted_slot_reads_as_none_but_others_survive() -> Result<(), Box<dyn Error>> {
    let (pool, _dir) = open_pool()?;
    let (_page_id, mut guard) = pool.new_page(TXN)?;
    let mut slotted = SlottedPage::new(&mut guard, TXN);
    slotted.init()?;

    let Some(a) = slotted.insert(b"first")? else {
        panic!("first tuple should fit in an empty page");
    };
    let Some(b) = slotted.insert(b"second")? else {
        panic!("second tuple should fit in an empty page");
    };

    slotted.delete(a)?;

    assert_eq!(slotted.read(a)?, None);
    assert_eq!(slotted.read(b)?, Some(b"second".as_slice()));
    Ok(())
}
