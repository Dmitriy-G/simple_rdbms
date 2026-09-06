use std::error::Error;

use common::{Lsn, PageId, Rid, TxnId};
use storage::buffer::BufferPool;
use storage::recovery;
use storage::wal::LogRecordKind;
use test_support::PoolOptions;
use txn::{IsolationLevel, TransactionManager, write_checkpoint};

fn open_pool(dir: &std::path::Path) -> Result<BufferPool, Box<dyn Error>> {
    test_support::open_pool(dir, PoolOptions::new(8))
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

#[test]
fn a_transaction_begun_after_a_commit_has_a_strictly_greater_read_ts() -> Result<(), Box<dyn Error>>
{
    let dir = tempfile::tempdir()?;
    let pool = open_pool(dir.path())?;
    let mut manager = TransactionManager::new(None);

    let earlier = manager.begin(&pool, IsolationLevel::ReadCommitted)?;
    let read_ts_before = manager.get(earlier)?.read_ts;
    manager.commit(earlier, &pool)?;

    let later = manager.begin(&pool, IsolationLevel::ReadCommitted)?;
    let read_ts_after = manager.get(later)?.read_ts;

    assert!(
        read_ts_after > read_ts_before,
        "a transaction begun after another commits ({read_ts_after}) must have a strictly \
         greater read_ts than the one begun before it ({read_ts_before})"
    );
    Ok(())
}

#[test]
fn a_committed_writers_version_is_visible_only_to_readers_begun_at_or_after_its_commit()
-> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let pool = open_pool(dir.path())?;
    let mut manager = TransactionManager::new(None);
    let rid = Rid::new(PageId(0), 0);

    let reader_before = manager.begin(&pool, IsolationLevel::ReadCommitted)?;
    let read_ts_before = manager.get(reader_before)?.read_ts;

    let writer = manager.begin(&pool, IsolationLevel::ReadCommitted)?;
    manager.version_store().record_insert(writer, rid);
    assert!(!manager.version_store().is_visible(rid, read_ts_before));

    manager.commit(writer, &pool)?;
    assert!(
        !manager.version_store().is_visible(rid, read_ts_before),
        "a transaction that began before the writer committed must not see its row"
    );

    let reader_after = manager.begin(&pool, IsolationLevel::ReadCommitted)?;
    let read_ts_after = manager.get(reader_after)?.read_ts;
    assert!(
        manager.version_store().is_visible(rid, read_ts_after),
        "a transaction begun after the writer committed must see its row"
    );
    Ok(())
}

#[test]
fn oldest_active_read_ts_tracks_the_active_set() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let pool = open_pool(dir.path())?;
    let mut manager = TransactionManager::new(None);

    let next_ts_before_any_txn = manager.oldest_active_read_ts();

    let older = manager.begin(&pool, IsolationLevel::ReadCommitted)?;
    let older_read_ts = manager.get(older)?.read_ts;
    assert_eq!(
        manager.oldest_active_read_ts(),
        next_ts_before_any_txn,
        "with only the older transaction active, the watermark is its own read_ts, which was \
         the next timestamp to be issued before it began"
    );

    let younger = manager.begin(&pool, IsolationLevel::ReadCommitted)?;
    let younger_read_ts = manager.get(younger)?.read_ts;
    assert!(younger_read_ts > older_read_ts);
    assert_eq!(
        manager.oldest_active_read_ts(),
        older_read_ts,
        "with two transactions open, the watermark is the older one's read_ts"
    );

    manager.commit(older, &pool)?;
    assert_eq!(
        manager.oldest_active_read_ts(),
        younger_read_ts,
        "once the older transaction leaves the active set, the watermark is the survivor's \
         read_ts"
    );

    manager.commit(younger, &pool)?;
    assert!(
        manager.oldest_active_read_ts() > younger_read_ts,
        "with nothing active again, the watermark is the next timestamp to be issued, which \
         must exceed every read_ts already handed out"
    );
    Ok(())
}

#[test]
fn an_aborted_writers_version_never_resurfaces_for_a_later_committed_writer()
-> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let pool = open_pool(dir.path())?;
    let mut manager = TransactionManager::new(None);
    let rid = Rid::new(PageId(0), 0);

    let aborted_writer = manager.begin(&pool, IsolationLevel::ReadCommitted)?;
    manager.version_store().record_insert(aborted_writer, rid);
    manager.abort(aborted_writer, &pool)?;

    let later_writer = manager.begin(&pool, IsolationLevel::ReadCommitted)?;
    manager.version_store().record_insert(later_writer, rid);

    let reader_before_commit = manager.begin(&pool, IsolationLevel::ReadCommitted)?;
    let read_ts_before_commit = manager.get(reader_before_commit)?.read_ts;
    assert!(!manager.version_store().is_visible(rid, read_ts_before_commit));

    manager.commit(later_writer, &pool)?;
    let reader_after_commit = manager.begin(&pool, IsolationLevel::ReadCommitted)?;
    let read_ts_after_commit = manager.get(reader_after_commit)?.read_ts;
    assert!(
        manager.version_store().is_visible(rid, read_ts_after_commit),
        "the later writer's own committed version must be visible - the aborted writer's \
         version must not still be occupying (or otherwise poisoning) the chain"
    );
    Ok(())
}

#[test]
fn commit_with_no_other_transaction_active_prunes_the_chain_immediately()
-> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let pool = open_pool(dir.path())?;
    let mut manager = TransactionManager::new(None);
    let rid = Rid::new(PageId(0), 0);

    let writer = manager.begin(&pool, IsolationLevel::ReadCommitted)?;
    manager.version_store().record_insert(writer, rid);
    manager.commit(writer, &pool)?;

    assert_eq!(
        manager.version_store().chain_count(),
        0,
        "with nothing else active, the watermark already covers the commit_ts"
    );
    Ok(())
}

#[test]
fn committed_chain_survives_an_older_reader_and_is_pruned_once_that_reader_ends()
-> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let pool = open_pool(dir.path())?;
    let mut manager = TransactionManager::new(None);
    let rid = Rid::new(PageId(0), 0);

    let reader = manager.begin(&pool, IsolationLevel::ReadCommitted)?;
    let writer = manager.begin(&pool, IsolationLevel::ReadCommitted)?;
    manager.version_store().record_insert(writer, rid);
    manager.commit(writer, &pool)?;

    assert_eq!(
        manager.version_store().chain_count(),
        1,
        "the older reader's snapshot predates the commit, so the watermark cannot reach it yet"
    );

    manager.commit(reader, &pool)?;
    assert_eq!(
        manager.version_store().chain_count(),
        0,
        "once the older reader leaves the active set, the watermark reaches commit_ts and the \
         chain is prunable"
    );
    Ok(())
}

#[test]
fn a_row_committed_before_a_reader_began_stays_visible_after_its_chain_is_pruned()
-> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let pool = open_pool(dir.path())?;
    let mut manager = TransactionManager::new(None);
    let rid = Rid::new(PageId(0), 0);

    let writer = manager.begin(&pool, IsolationLevel::ReadCommitted)?;
    manager.version_store().record_insert(writer, rid);
    manager.commit(writer, &pool)?;
    assert_eq!(
        manager.version_store().chain_count(),
        0,
        "nothing else was active, so the chain is pruned as part of this commit"
    );

    let reader = manager.begin(&pool, IsolationLevel::ReadCommitted)?;
    let read_ts = manager.get(reader)?.read_ts;
    assert!(
        manager.version_store().is_visible(rid, read_ts),
        "no chain at all means visible to everyone, which is exactly right for a row committed \
         before this reader ever began"
    );
    Ok(())
}

#[test]
fn abort_with_no_other_transaction_active_prunes_the_emptied_chain_immediately()
-> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let pool = open_pool(dir.path())?;
    let mut manager = TransactionManager::new(None);
    let rid = Rid::new(PageId(0), 0);

    let writer = manager.begin(&pool, IsolationLevel::ReadCommitted)?;
    manager.version_store().record_insert(writer, rid);
    manager.abort(writer, &pool)?;

    assert_eq!(
        manager.version_store().chain_count(),
        0,
        "with nothing else active, the watermark already covers the abort's own stamp"
    );
    Ok(())
}

#[test]
fn emptied_chain_survives_an_older_reader_and_is_pruned_once_that_reader_ends()
-> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let pool = open_pool(dir.path())?;
    let mut manager = TransactionManager::new(None);
    let rid = Rid::new(PageId(0), 0);

    let reader = manager.begin(&pool, IsolationLevel::ReadCommitted)?;
    let writer = manager.begin(&pool, IsolationLevel::ReadCommitted)?;
    manager.version_store().record_insert(writer, rid);
    manager.abort(writer, &pool)?;

    assert_eq!(
        manager.version_store().chain_count(),
        1,
        "the older reader was active when the abort ran, so the emptied chain must survive - a \
         reader that read the row's bytes before the undo ran may still be resolving them"
    );

    manager.commit(reader, &pool)?;
    assert_eq!(
        manager.version_store().chain_count(),
        0,
        "once the older reader leaves the active set, the watermark reaches the abort's stamp \
         and the emptied chain is prunable"
    );
    Ok(())
}
