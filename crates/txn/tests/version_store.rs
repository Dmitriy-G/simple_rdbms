use common::{PageId, Rid, TxnId};
use txn::VersionStore;

fn rid(slot: u16) -> Rid {
    Rid::new(PageId(0), slot)
}

#[test]
fn unknown_rid_is_visible_at_any_read_ts() {
    let store = VersionStore::new();
    let r = rid(1);

    assert!(store.is_visible(r, 0));
    assert!(store.is_visible(r, 1_000));
}

#[test]
fn recorded_but_uncommitted_insert_is_invisible() {
    let store = VersionStore::new();
    let r = rid(1);

    store.record_insert(TxnId(1), r);

    assert!(!store.is_visible(r, 0));
    assert!(!store.is_visible(r, 1_000));
}

#[test]
fn committed_insert_is_visible_at_and_after_its_commit_ts() {
    let store = VersionStore::new();
    let r = rid(1);

    store.record_insert(TxnId(1), r);
    store.commit_versions(TxnId(1), 5);

    assert!(!store.is_visible(r, 4));
    assert!(store.is_visible(r, 5));
    assert!(store.is_visible(r, 6));
}

#[test]
fn aborted_insert_does_not_resurface_once_another_transaction_commits_a_new_version() {
    let store = VersionStore::new();
    let r = rid(1);

    store.record_insert(TxnId(1), r);
    store.abort_versions(TxnId(1), 0);

    store.record_insert(TxnId(2), r);
    assert!(!store.is_visible(r, 9));

    store.commit_versions(TxnId(2), 10);
    assert!(!store.is_visible(r, 9));
    assert!(store.is_visible(r, 10));
}

#[test]
fn aborted_insert_stays_invisible_immediately_after_the_abort() {
    let store = VersionStore::new();
    let r = rid(1);

    store.record_insert(TxnId(1), r);
    store.abort_versions(TxnId(1), 0);

    assert!(!store.is_visible(r, 100));
    assert!(!store.is_visible_to(r, TxnId(1), 100));
}

#[test]
fn different_transactions_recording_different_rids_do_not_affect_each_other() {
    let store = VersionStore::new();
    let r1 = rid(1);
    let r2 = rid(2);

    store.record_insert(TxnId(1), r1);
    store.record_insert(TxnId(2), r2);
    store.commit_versions(TxnId(1), 5);

    assert!(store.is_visible(r1, 5));
    assert!(!store.is_visible(r2, 5));

    store.commit_versions(TxnId(2), 7);
    assert!(store.is_visible(r1, 5));
    assert!(store.is_visible(r2, 7));
}

#[test]
fn is_visible_to_shows_the_readers_own_uncommitted_insert() {
    let store = VersionStore::new();
    let r = rid(1);

    store.record_insert(TxnId(1), r);

    assert!(store.is_visible_to(r, TxnId(1), 0));
    assert!(!store.is_visible_to(r, TxnId(2), 0));
}

#[test]
fn is_visible_to_matches_is_visible_for_committed_rows() {
    let store = VersionStore::new();
    let r = rid(1);

    store.record_insert(TxnId(1), r);
    store.commit_versions(TxnId(1), 5);

    assert!(!store.is_visible_to(r, TxnId(2), 4));
    assert!(store.is_visible_to(r, TxnId(2), 5));
}

#[test]
fn chain_count_tracks_one_chain_per_distinct_rid_recorded() {
    let store = VersionStore::new();
    const ROW_COUNT: u16 = 20;

    for slot in 0..ROW_COUNT {
        store.record_insert(TxnId(slot as u64), rid(slot));
        store.commit_versions(TxnId(slot as u64), slot as u64);
    }

    assert_eq!(
        store.chain_count(),
        ROW_COUNT as usize,
        "one chain per distinct Rid recorded, however many transactions committed it"
    );

    store.record_insert(TxnId(1_000), rid(0));
    store.commit_versions(TxnId(1_000), 1_000);
    assert_eq!(
        store.chain_count(),
        ROW_COUNT as usize,
        "a second version recorded against an already-seen Rid must not grow the map - it \
         extends that Rid's existing chain"
    );
}

#[test]
fn abort_leaves_the_emptied_chain_in_place_but_invisible() {
    let store = VersionStore::new();
    let r = rid(1);

    store.record_insert(TxnId(1), r);
    let count_after_insert = store.chain_count();

    store.abort_versions(TxnId(1), 0);

    assert_eq!(
        store.chain_count(),
        count_after_insert,
        "abort_versions must not remove the chain, only empty it - pruning it is subtask 3's job"
    );
    assert!(!store.is_visible(r, 100));
    assert!(!store.is_visible_to(r, TxnId(1), 100));
}

#[test]
fn committing_one_writer_does_not_expose_a_concurrent_writers_chain() {
    let store = VersionStore::new();
    let r1 = rid(1);
    let r2 = rid(2);

    store.record_insert(TxnId(1), r1);
    store.record_insert(TxnId(2), r2);

    store.commit_versions(TxnId(1), 5);

    assert!(store.is_visible(r1, 5));
    assert!(!store.is_visible(r2, 5));
    assert!(store.is_visible_to(r2, TxnId(2), 5));
}

#[test]
fn commit_and_abort_of_an_empty_write_set_leave_other_chains_untouched() {
    let store = VersionStore::new();
    let r = rid(1);

    store.record_insert(TxnId(1), r);
    store.commit_versions(TxnId(1), 5);

    store.commit_versions(TxnId(2), 6);
    store.abort_versions(TxnId(3), 100);

    assert!(store.is_visible(r, 5));
    assert_eq!(store.chain_count(), 1);
}

#[test]
fn prune_drops_a_committed_chain_at_or_below_the_watermark() {
    let store = VersionStore::new();
    let r = rid(1);

    store.record_insert(TxnId(1), r);
    store.commit_versions(TxnId(1), 5);
    assert_eq!(store.chain_count(), 1);

    store.prune(5);

    assert_eq!(store.chain_count(), 0);
    assert!(store.is_visible(r, 100), "no chain at all means visible to everyone");
}

#[test]
fn prune_leaves_a_committed_chain_above_the_watermark() {
    let store = VersionStore::new();
    let r = rid(1);

    store.record_insert(TxnId(1), r);
    store.commit_versions(TxnId(1), 10);

    store.prune(5);

    assert_eq!(store.chain_count(), 1);
    assert!(store.is_visible(r, 10));
}

#[test]
fn prune_rechecks_a_candidate_that_gained_a_new_version_since_it_was_enrolled() {
    let store = VersionStore::new();
    let r = rid(1);

    store.record_insert(TxnId(1), r);
    store.abort_versions(TxnId(1), 5);

    store.record_insert(TxnId(2), r);
    store.commit_versions(TxnId(2), 7);

    store.prune(5);
    assert_eq!(
        store.chain_count(),
        1,
        "the chain gained a new, not-yet-prunable version after the abort enrolled it - the \
         stale candidate entry must not drop it"
    );
    assert!(!store.is_visible(r, 5));

    store.prune(7);
    assert_eq!(
        store.chain_count(),
        0,
        "once the watermark reaches the second writer's own commit_ts, the chain is prunable \
         on its own account"
    );
    assert!(store.is_visible(r, 100));
}
