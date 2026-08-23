//! `storage::recovery`: Analysis/Redo/Undo exercised directly against
//! hand-built logs, without any transaction-manager or catalog involved
//! (recovery doesn't know about either). Each test opens a `BufferPool`,
//! does some writes and explicit log bookkeeping to set up a scenario,
//! drops it without a clean shutdown (simulating a crash), then reopens a
//! fresh `BufferPool` over the same files and calls `recovery::recover`.

use std::error::Error;
use std::path::Path;

use common::{Lsn, TxnId};
use storage::buffer::BufferPool;
use storage::disk::DiskManager;
use storage::page::PAGE_SIZE;
use storage::recovery;
use storage::replacer::LruKReplacer;
use storage::wal::{CHECKPOINT_TXN, LogManager, LogRecordKind};

fn open_pool(dir: &Path, pool_size: usize) -> Result<BufferPool, Box<dyn Error>> {
    let disk = DiskManager::open(dir.join("test.db"), PAGE_SIZE)?;
    let log = LogManager::open(dir.join("test.db.wal"))?;
    Ok(BufferPool::new(disk, log, pool_size, Box::new(LruKReplacer::new(pool_size, 2))))
}

#[test]
fn redo_replays_committed_writes_that_never_reached_disk() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;

    let page_id;
    {
        let pool = open_pool(dir.path(), 8)?;
        let (pid, mut guard) = pool.new_page(TxnId(1))?;
        page_id = pid;
        guard.write(TxnId(1), 16, b"hello!")?;
        drop(guard);
        let commit_lsn = pool.append_log(TxnId(1), LogRecordKind::Commit)?;
        pool.flush_log(commit_lsn)?;
        // Deliberately no `flush_all`/`sync`: the commit is durable in the
        // log, but the page itself never reached disk - exactly what
        // no-force allows, and exactly what a crash right here would leave
        // behind.
    }

    let pool = open_pool(dir.path(), 8)?;
    recovery::recover(&pool)?;

    let guard = pool.fetch_page(page_id)?;
    assert_eq!(&guard.page().data()[16..22], b"hello!");
    Ok(())
}

#[test]
fn undo_rolls_back_an_uncommitted_transactions_writes() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;

    let page_id;
    {
        let pool = open_pool(dir.path(), 8)?;
        let (pid, mut guard) = pool.new_page(TxnId(1))?;
        page_id = pid;
        guard.write(TxnId(1), 16, b"before")?;
        drop(guard);
        let commit_lsn = pool.append_log(TxnId(1), LogRecordKind::Commit)?;
        pool.flush_log(commit_lsn)?;
        pool.flush_all()?;

        // Txn 2 overwrites the same bytes and even reaches disk (steal),
        // but never commits.
        let mut guard = pool.fetch_page(page_id)?;
        guard.write(TxnId(2), 16, b"after!")?;
        drop(guard);
        pool.flush_log_all()?;
        pool.flush_all()?;
    }

    let pool = open_pool(dir.path(), 8)?;
    recovery::recover(&pool)?;

    let guard = pool.fetch_page(page_id)?;
    assert_eq!(&guard.page().data()[16..22], b"before", "the uncommitted write must be undone");
    Ok(())
}

#[test]
fn recovery_is_idempotent_when_run_twice() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;

    let page_id;
    {
        let pool = open_pool(dir.path(), 8)?;
        let (pid, mut guard) = pool.new_page(TxnId(1))?;
        page_id = pid;
        guard.write(TxnId(1), 16, b"hello!")?;
        drop(guard);
        let commit_lsn = pool.append_log(TxnId(1), LogRecordKind::Commit)?;
        pool.flush_log(commit_lsn)?;
    }

    let pool = open_pool(dir.path(), 8)?;
    recovery::recover(&pool)?;
    let after_first = pool.fetch_page(page_id)?.page().data().to_vec();

    recovery::recover(&pool)?;
    let after_second = pool.fetch_page(page_id)?.page().data().to_vec();

    assert_eq!(after_first, after_second, "redo must be idempotent");
    Ok(())
}

#[test]
fn undo_resumes_from_a_partially_undone_transaction_without_redoing_a_completed_step()
-> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;

    let page_id;
    {
        let pool = open_pool(dir.path(), 8)?;
        let (pid, mut guard) = pool.new_page(TxnId(1))?;
        page_id = pid;
        guard.write(TxnId(1), 16, b"000000")?;
        drop(guard);
        let commit_lsn = pool.append_log(TxnId(1), LogRecordKind::Commit)?;
        pool.flush_log(commit_lsn)?;
        pool.flush_all()?;

        // Txn 2: two updates to the same bytes, never committed.
        let mut guard = pool.fetch_page(page_id)?;
        guard.write(TxnId(2), 16, b"111111")?;
        drop(guard);

        let mut guard = pool.fetch_page(page_id)?;
        guard.write(TxnId(2), 16, b"222222")?;
        let second_update_lsn = guard.page().page_lsn();
        drop(guard);
        pool.flush_log_all()?;
        pool.flush_all()?;

        // Simulate a first recovery attempt that undid exactly the second
        // update (txn 2's most recent record) and then crashed again
        // before writing Abort/End.
        let second_update =
            pool.log_iter_from(second_update_lsn)?.next().ok_or("expected a record at this LSN")?;
        let LogRecordKind::Update { offset, before, .. } = second_update.kind else {
            return Err("expected the second update's own record back".into());
        };
        let undo_next_lsn =
            second_update.prev_lsn.ok_or("expected txn 2 to have a first update")?;
        let clr_lsn = pool.append_log(
            TxnId(2),
            LogRecordKind::Clr { page_id, offset, after: before.clone(), undo_next_lsn },
        )?;
        pool.stamp_write(page_id, offset as usize, &before, clr_lsn)?;
        pool.flush_log_all()?;
        pool.flush_all()?;
    }

    let pool = open_pool(dir.path(), 8)?;
    recovery::recover(&pool)?;

    let guard = pool.fetch_page(page_id)?;
    assert_eq!(&guard.page().data()[16..22], b"000000", "both updates must end up undone");
    drop(guard);

    let clrs = pool
        .log_iter_from(Lsn(1))?
        .filter(|r| r.txn_id == TxnId(2) && matches!(r.kind, LogRecordKind::Clr { .. }))
        .count();
    assert_eq!(
        clrs, 2,
        "one Clr from the simulated first attempt, one from resuming - never a third \
         re-undoing the already-undone second update"
    );
    Ok(())
}

#[test]
fn analysis_starts_from_the_checkpoint_and_still_finds_a_loser_that_began_before_it()
-> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;

    let page_id;
    {
        let pool = open_pool(dir.path(), 8)?;
        let (pid, mut guard) = pool.new_page(TxnId(1))?;
        page_id = pid;
        guard.write(TxnId(1), 16, b"000000")?;
        drop(guard);
        let commit_lsn = pool.append_log(TxnId(1), LogRecordKind::Commit)?;
        pool.flush_log(commit_lsn)?;
        pool.flush_all()?;

        // Txn 2's first update, before the checkpoint.
        let mut guard = pool.fetch_page(page_id)?;
        guard.write(TxnId(2), 16, b"111111")?;
        let first_update_lsn = guard.page().page_lsn();
        drop(guard);

        // A checkpoint capturing txn 2 as still active, with the page it
        // dirtied.
        let begin_lsn = pool.append_log(CHECKPOINT_TXN, LogRecordKind::CheckpointBegin)?;
        pool.append_log(
            CHECKPOINT_TXN,
            LogRecordKind::CheckpointEnd {
                att: vec![(TxnId(2), first_update_lsn)],
                dpt: vec![(page_id, first_update_lsn)],
            },
        )?;
        pool.set_last_checkpoint_lsn(begin_lsn)?;
        pool.flush_log_all()?;

        // Txn 2's second update, after the checkpoint - still never
        // committed.
        let mut guard = pool.fetch_page(page_id)?;
        guard.write(TxnId(2), 16, b"222222")?;
        drop(guard);
        pool.flush_log_all()?;
        pool.flush_all()?;
    }

    let pool = open_pool(dir.path(), 8)?;
    recovery::recover(&pool)?;

    let guard = pool.fetch_page(page_id)?;
    assert_eq!(
        &guard.page().data()[16..22],
        b"000000",
        "the pre-checkpoint update must still be undone even though analysis started scanning \
         only from the checkpoint's LSN"
    );
    Ok(())
}

#[test]
fn begin_resets_stale_committed_state_left_by_a_reused_txn_id() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;

    let page_id;
    {
        let pool = open_pool(dir.path(), 8)?;

        // A page to write into, allocated and committed under an unrelated
        // transaction, so the story below is exactly `Begin`/`Update`/
        // `Commit` for txn 5, nothing more.
        let (pid, mut guard) = pool.new_page(TxnId(1))?;
        page_id = pid;
        guard.write(TxnId(1), 16, b"------")?;
        drop(guard);
        let setup_commit = pool.append_log(TxnId(1), LogRecordKind::Commit)?;
        pool.flush_log(setup_commit)?;
        pool.append_log(TxnId(1), LogRecordKind::End)?;
        pool.flush_log_all()?;
        pool.flush_all()?;

        // Generation 1: Begin(5), Update(5), Commit(5) - deliberately no
        // `End`, exactly what a crash between the flushed `Commit` and the
        // unflushed `End` would leave behind.
        pool.append_log(TxnId(5), LogRecordKind::Begin)?;
        let mut guard = pool.fetch_page(page_id)?;
        guard.write(TxnId(5), 16, b"first!")?;
        drop(guard);
        let commit_lsn = pool.append_log(TxnId(5), LogRecordKind::Commit)?;
        pool.flush_log(commit_lsn)?;

        // Generation 2 reuses id 5 - exactly what an unseeded
        // `TransactionManager` would hand out next: Begin(5), Update(5), and
        // nothing more (no Commit, no End). Without Analysis resetting the
        // ATT entry on `Begin`, this write would inherit generation 1's
        // stale `committed: true` and never get undone.
        pool.append_log(TxnId(5), LogRecordKind::Begin)?;
        let mut guard = pool.fetch_page(page_id)?;
        guard.write(TxnId(5), 16, b"second")?;
        drop(guard);
        pool.flush_log_all()?;
        pool.flush_all()?;
    }

    let pool = open_pool(dir.path(), 8)?;
    recovery::recover(&pool)?;

    let guard = pool.fetch_page(page_id)?;
    assert_eq!(
        &guard.page().data()[16..22],
        b"first!",
        "generation 1's committed write must survive, and generation 2's uncommitted reuse of \
         the same id must be undone rather than left in place"
    );
    Ok(())
}
