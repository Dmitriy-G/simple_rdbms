use common::TxnId;
use txn::{VersionChain, VersionEntry};

fn entry(creator: u64, begin_ts: Option<u64>, end_ts: Option<u64>) -> VersionEntry {
    VersionEntry { creator_txn_id: TxnId(creator), begin_ts, end_ts }
}

#[test]
fn uncommitted_entry_is_never_visible() {
    let mut chain = VersionChain::new();
    chain.push(entry(1, None, None));

    assert!(chain.visible_version(0).is_none());
    assert!(chain.visible_version(100).is_none());
}

#[test]
fn committed_entry_is_visible_at_or_after_begin_ts() {
    let mut chain = VersionChain::new();
    chain.push(entry(1, Some(5), None));

    assert!(chain.visible_version(4).is_none());
    assert!(chain.visible_version(5).is_some());
    assert!(chain.visible_version(6).is_some());
}

#[test]
fn two_committed_entries_pick_the_newest_covering_begin_ts() {
    let mut chain = VersionChain::new();
    chain.push(entry(1, Some(5), Some(10)));
    chain.push(entry(2, Some(10), None));

    assert!(chain.visible_version(4).is_none());

    let at_five = chain.visible_version(5).expect("visible at begin_ts of the older version");
    assert_eq!(at_five.creator_txn_id, TxnId(1));

    let at_nine = chain.visible_version(9).expect("visible up to end_ts of the older version");
    assert_eq!(at_nine.creator_txn_id, TxnId(1));

    let at_ten = chain.visible_version(10).expect("visible at begin_ts of the newer version");
    assert_eq!(at_ten.creator_txn_id, TxnId(2));

    let at_hundred = chain.visible_version(100).expect("still visible far in the future");
    assert_eq!(at_hundred.creator_txn_id, TxnId(2));
}

#[test]
fn entry_superseded_at_or_before_read_ts_is_invisible() {
    let mut chain = VersionChain::new();
    chain.push(entry(1, Some(5), Some(10)));

    assert!(chain.visible_version(10).is_none());
    assert!(chain.visible_version(11).is_none());
    assert!(chain.visible_version(9).is_some());
}

#[test]
fn empty_chain_returns_none() {
    let chain = VersionChain::new();
    assert!(chain.visible_version(0).is_none());
}

#[test]
fn visible_version_for_shows_the_readers_own_uncommitted_entry() {
    let mut chain = VersionChain::new();
    chain.push(entry(7, None, None));

    assert!(chain.visible_version_for(TxnId(7), 0).is_some());
    assert!(chain.visible_version_for(TxnId(7), 1_000).is_some());
}

#[test]
fn visible_version_for_still_hides_another_transactions_uncommitted_entry() {
    let mut chain = VersionChain::new();
    chain.push(entry(7, None, None));

    assert!(chain.visible_version_for(TxnId(8), 0).is_none());
    assert!(chain.visible_version_for(TxnId(8), 1_000).is_none());
}

#[test]
fn visible_version_for_falls_back_to_the_ordinary_rule_for_committed_entries() {
    let mut chain = VersionChain::new();
    chain.push(entry(1, Some(5), None));

    assert!(chain.visible_version_for(TxnId(2), 4).is_none());
    assert!(chain.visible_version_for(TxnId(2), 5).is_some());
}
