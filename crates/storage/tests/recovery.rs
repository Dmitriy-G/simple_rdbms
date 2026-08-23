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
        // Recording the checkpoint's own LSN in the header (page 0) is a
        // real, logged page mutation now, so it needs its own committed
        // transaction - matches `txn::write_checkpoint`'s own use of a
        // dedicated header transaction.
        pool.set_last_checkpoint_lsn(TxnId(9), begin_lsn)?;
        let header_commit_lsn = pool.append_log(TxnId(9), LogRecordKind::Commit)?;
        pool.flush_log(header_commit_lsn)?;
        pool.append_log(TxnId(9), LogRecordKind::End)?;
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

/// Page 0's header fields (`catalog_first_page`, `last_checkpoint_lsn`)
/// must be exactly as redoable and undoable as any other page's bytes -
/// per `task.MD`'s "route page 0 through the WAL". A raw, unlogged write
/// (the bug this guards against) could never be rolled back this way: only
/// a properly WAL-tracked `Update` record lets `undo_transaction` find and
/// revert it.
#[test]
fn an_uncommitted_header_update_is_undone_like_any_other_page_mutation()
-> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    {
        let pool = open_pool(dir.path(), 8)?;
        assert_eq!(pool.catalog_first_page()?, None, "no catalog heap yet");

        // A transaction that allocates a heap page and points the header at
        // it, but crashes before ever committing.
        pool.append_log(TxnId(1), LogRecordKind::Begin)?;
        let (page_id, guard) = pool.new_page(TxnId(1))?;
        drop(guard);
        pool.set_catalog_first_page(TxnId(1), page_id)?;
        pool.flush_log_all()?;
        pool.flush_all()?;
    }

    let pool = open_pool(dir.path(), 8)?;
    recovery::recover(&pool)?;

    assert_eq!(
        pool.catalog_first_page()?,
        None,
        "an uncommitted header update must be undone like any other page mutation"
    );
    Ok(())
}

/// A transaction that commits and ends *during* the checkpoint window
/// (between `CheckpointBegin` and `CheckpointEnd`) must not get resurrected
/// as still-active by the checkpoint's own snapshot, which was captured
/// slightly earlier and is stale by the time `CheckpointEnd` is actually
/// logged. Driving `LogManager` directly, since the engine can't currently
/// produce this exact interleaving.
#[test]
fn checkpoint_seeding_does_not_resurrect_a_transaction_the_scan_already_saw_end_for()
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
        pool.append_log(TxnId(1), LogRecordKind::End)?;
        pool.flush_all()?;

        // Txn 2 begins and writes, before the checkpoint starts.
        pool.append_log(TxnId(2), LogRecordKind::Begin)?;
        let mut guard = pool.fetch_page(page_id)?;
        guard.write(TxnId(2), 16, b"222222")?;
        let update_lsn = guard.page().page_lsn();
        drop(guard);

        // The checkpoint begins - a fuzzy snapshot will be taken "around"
        // this point, but `CheckpointEnd` (carrying it) isn't logged until
        // later.
        let begin_lsn = pool.append_log(CHECKPOINT_TXN, LogRecordKind::CheckpointBegin)?;

        // Interleaved between `CheckpointBegin` and `CheckpointEnd`: txn 2
        // actually commits and ends.
        let commit_lsn = pool.append_log(TxnId(2), LogRecordKind::Commit)?;
        pool.flush_log(commit_lsn)?;
        pool.append_log(TxnId(2), LogRecordKind::End)?;

        // `CheckpointEnd`'s own snapshot is stale by the time it's logged:
        // it still shows txn 2 active, exactly as it genuinely was when the
        // snapshot was captured - just before the interleaved commit above.
        pool.append_log(
            CHECKPOINT_TXN,
            LogRecordKind::CheckpointEnd {
                att: vec![(TxnId(2), update_lsn)],
                dpt: vec![(page_id, update_lsn)],
            },
        )?;

        pool.set_last_checkpoint_lsn(TxnId(9), begin_lsn)?;
        let header_commit_lsn = pool.append_log(TxnId(9), LogRecordKind::Commit)?;
        pool.flush_log(header_commit_lsn)?;
        pool.append_log(TxnId(9), LogRecordKind::End)?;

        pool.flush_log_all()?;
        pool.flush_all()?;
    }

    let pool = open_pool(dir.path(), 8)?;
    recovery::recover(&pool)?;

    let guard = pool.fetch_page(page_id)?;
    assert_eq!(
        &guard.page().data()[16..22],
        b"222222",
        "txn 2's committed write must survive recovery - the checkpoint's own snapshot, \
         captured before txn 2's commit/end were logged, must not resurrect it as still active \
         and get it wrongly undone"
    );
    Ok(())
}
