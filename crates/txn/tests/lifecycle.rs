use std::error::Error;

use common::{Lsn, TxnId};
use storage::buffer::BufferPool;
use storage::disk::DiskManager;
use storage::dwb::DoubleWriteBuffer;
use storage::page::PAGE_SIZE;
use storage::recovery;
use storage::replacer::LruKReplacer;
use storage::wal::{LogManager, LogRecordKind};
use txn::{IsolationLevel, TransactionManager, write_checkpoint};

fn open_pool(dir: &std::path::Path) -> Result<BufferPool, Box<dyn Error>> {
    let disk = DiskManager::open(dir.join("test.db"), PAGE_SIZE)?;
    let dwb =
        DoubleWriteBuffer::open(dir.join("test.db.dwb"), DoubleWriteBuffer::DEFAULT_CAPACITY)?;
    let log = LogManager::open(dir.join("test.db.wal"))?;
    Ok(BufferPool::new(disk, dwb, log, 8, Box::new(LruKReplacer::new(8, 2))))
}

#[test]
fn begin_assigns_a_fresh_id_and_commit_removes_it_from_active() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let pool = open_pool(dir.path())?;
    let mut manager = TransactionManager::new(None);

    let txn_a = manager.begin(&pool, IsolationLevel::ReadCommitted)?;
    let txn_b = manager.begin(&pool, IsolationLevel::ReadCommitted)?;
    assert_ne!(txn_a, txn_b, "each begin must assign a distinct id");
    assert!(manager.get(txn_a).is_ok());

    manager.commit(txn_a, &pool)?;
    assert!(manager.get(txn_a).is_err(), "a committed transaction is no longer active");
    assert!(manager.get(txn_b).is_ok(), "committing one transaction must not affect another");
    Ok(())
}

#[test]
fn abort_undoes_writes_via_the_shared_undo_routine() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let pool = open_pool(dir.path())?;
    let mut manager = TransactionManager::new(None);

    let setup = manager.begin(&pool, IsolationLevel::ReadCommitted)?;
    let (page_id, mut guard) = pool.new_page(setup)?;
    guard.write(setup, 16, b"before")?;
    drop(guard);
    manager.commit(setup, &pool)?;

    let txn = manager.begin(&pool, IsolationLevel::ReadCommitted)?;
    let mut guard = pool.fetch_page(page_id)?;
    guard.write(txn, 16, b"after!")?;
    drop(guard);

    manager.abort(txn, &pool)?;
    assert!(manager.get(txn).is_err());

    let guard = pool.fetch_page(page_id)?;
    assert_eq!(&guard.page().data()[16..22], b"before", "abort must restore the before-image");
    Ok(())
}

#[test]
fn write_checkpoint_captures_active_transactions_and_dirty_pages() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let pool = open_pool(dir.path())?;
    let mut manager = TransactionManager::new(None);

    assert_eq!(pool.last_checkpoint_lsn()?, None);

    let txn = manager.begin(&pool, IsolationLevel::ReadCommitted)?;
    let (page_id, mut guard) = pool.new_page(txn)?;
    guard.write(txn, 16, b"hello!")?;
    drop(guard);

    let begin_lsn = write_checkpoint(&pool, &mut manager)?;
    assert_eq!(pool.last_checkpoint_lsn()?, Some(begin_lsn));

    let dpt = pool.dirty_page_table();
    assert!(dpt.iter().any(|(id, _)| *id == page_id), "the dirty page must be in the DPT snapshot");

    let att = manager.active_snapshot(&pool);
    assert!(
        att.iter().any(|(id, lsn)| *id == txn && *lsn != Lsn(0)),
        "the active txn must be in the ATT snapshot"
    );
    Ok(())
}

#[test]
fn commit_of_an_unknown_transaction_is_an_error() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let pool = open_pool(dir.path())?;
    let mut manager = TransactionManager::new(None);
    assert!(manager.commit(TxnId(999), &pool).is_err());
    Ok(())
}

#[test]
fn recovery_seeds_the_next_id_past_every_id_the_log_has_ever_used() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    {
        let pool = open_pool(dir.path())?;
        pool.append_log(TxnId(7), LogRecordKind::Begin)?;
        let commit_lsn = pool.append_log(TxnId(7), LogRecordKind::Commit)?;
        pool.flush_log(commit_lsn)?;
        pool.append_log(TxnId(7), LogRecordKind::End)?;
        pool.flush_log_all()?;
    }

    let pool = open_pool(dir.path())?;
    let highest_seen = recovery::recover(&pool)?;
    assert_eq!(highest_seen, Some(TxnId(7)));

    let mut manager = TransactionManager::new(highest_seen);
    let txn = manager.begin(&pool, IsolationLevel::ReadCommitted)?;
    assert!(txn.0 > 7, "the next id handed out ({txn:?}) must exceed the log's highest id (7)");
    Ok(())
}
