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
    store.abort_versions(TxnId(1));

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
    store.abort_versions(TxnId(1));

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
