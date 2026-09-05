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

    store.record_insert(TxnId(1), r, b"row".to_vec());

    assert!(!store.is_visible(r, 0));
    assert!(!store.is_visible(r, 1_000));
}

#[test]
fn committed_insert_is_visible_at_and_after_its_commit_ts() {
    let store = VersionStore::new();
    let r = rid(1);

    store.record_insert(TxnId(1), r, b"row".to_vec());
    store.commit_versions(TxnId(1), 5);

    assert!(!store.is_visible(r, 4));
    assert!(store.is_visible(r, 5));
    assert!(store.is_visible(r, 6));
}

#[test]
fn aborted_insert_does_not_resurface_once_another_transaction_commits_a_new_version() {
    let store = VersionStore::new();
    let r = rid(1);

    store.record_insert(TxnId(1), r, b"row".to_vec());
    store.abort_versions(TxnId(1));

    store.record_insert(TxnId(2), r, b"other row".to_vec());
    assert!(!store.is_visible(r, 9));

    store.commit_versions(TxnId(2), 10);
    assert!(!store.is_visible(r, 9));
    assert!(store.is_visible(r, 10));
}

#[test]
fn aborted_insert_stays_invisible_immediately_after_the_abort() {
    let store = VersionStore::new();
    let r = rid(1);

    store.record_insert(TxnId(1), r, b"row".to_vec());
    store.abort_versions(TxnId(1));

    assert!(!store.is_visible(r, 100));
    assert!(!store.is_visible_to(r, TxnId(1), 100));
}

#[test]
fn different_transactions_recording_different_rids_do_not_affect_each_other() {
    let store = VersionStore::new();
    let r1 = rid(1);
    let r2 = rid(2);

    store.record_insert(TxnId(1), r1, b"one".to_vec());
    store.record_insert(TxnId(2), r2, b"two".to_vec());
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

    store.record_insert(TxnId(1), r, b"row".to_vec());

    assert!(store.is_visible_to(r, TxnId(1), 0));
    assert!(!store.is_visible_to(r, TxnId(2), 0));
}

#[test]
fn is_visible_to_matches_is_visible_for_committed_rows() {
    let store = VersionStore::new();
    let r = rid(1);

    store.record_insert(TxnId(1), r, b"row".to_vec());
    store.commit_versions(TxnId(1), 5);

    assert!(!store.is_visible_to(r, TxnId(2), 4));
    assert!(store.is_visible_to(r, TxnId(2), 5));
}
