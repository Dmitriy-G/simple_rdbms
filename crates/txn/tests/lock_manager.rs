use std::sync::Arc;
use std::sync::mpsc::{RecvTimeoutError, channel};
use std::thread;
use std::time::Duration;

use common::{PageId, Rid, TableId, TxnId};
use txn::{LockManager, LockMode, TxnError};

const TIMEOUT: Duration = Duration::from_millis(500);

fn rid(slot: u16) -> Rid {
    Rid::new(PageId(0), slot)
}

#[test]
fn two_shared_holders_coexist() {
    let locks = LockManager::new();
    let r = rid(1);
    assert!(locks.lock(TxnId(1), r, LockMode::Shared).is_ok());
    assert!(locks.lock(TxnId(2), r, LockMode::Shared).is_ok());
}

#[test]
fn table_locks_conflict_like_row_locks() {
    let locks = LockManager::new();
    let t = TableId(1);
    assert!(locks.lock_table(TxnId(1), t, LockMode::Shared).is_ok());
    assert!(locks.lock_table(TxnId(2), t, LockMode::Shared).is_ok());
    locks.release_all(TxnId(1));
    locks.release_all(TxnId(2));
    assert!(locks.lock_table(TxnId(3), t, LockMode::Exclusive).is_ok());
}

#[test]
fn exclusive_waits_for_shared_then_is_granted_after_release() {
    let locks = Arc::new(LockManager::new());
    let r = rid(1);
    assert!(locks.lock(TxnId(1), r, LockMode::Shared).is_ok());

    let (tx, rx) = channel();
    let waiter = Arc::clone(&locks);
    thread::spawn(move || {
        let _ = tx.send(waiter.lock(TxnId(2), r, LockMode::Exclusive));
    });

    match rx.recv_timeout(TIMEOUT) {
        Err(RecvTimeoutError::Timeout) => {}
        other => panic!("exclusive request must block behind a shared holder, got {other:?}"),
    }

    locks.release_all(TxnId(1));

    match rx.recv_timeout(TIMEOUT) {
        Ok(Ok(())) => {}
        other => panic!(
            "exclusive request must be granted once the shared lock is released, got {other:?}"
        ),
    }
}

#[test]
fn upgrade_succeeds_immediately_when_sole_holder() {
    let locks = LockManager::new();
    let r = rid(1);
    assert!(locks.lock(TxnId(1), r, LockMode::Shared).is_ok());
    assert!(locks.lock(TxnId(1), r, LockMode::Exclusive).is_ok());
}

#[test]
fn upgrade_waits_behind_another_shared_holder_then_succeeds() {
    let locks = Arc::new(LockManager::new());
    let r = rid(1);
    assert!(locks.lock(TxnId(1), r, LockMode::Shared).is_ok());
    assert!(locks.lock(TxnId(2), r, LockMode::Shared).is_ok());

    let (tx, rx) = channel();
    let upgrader = Arc::clone(&locks);
    thread::spawn(move || {
        let _ = tx.send(upgrader.lock(TxnId(1), r, LockMode::Exclusive));
    });

    match rx.recv_timeout(TIMEOUT) {
        Err(RecvTimeoutError::Timeout) => {}
        other => {
            panic!("upgrade must wait while another transaction still holds shared, got {other:?}")
        }
    }

    locks.release_all(TxnId(2));

    match rx.recv_timeout(TIMEOUT) {
        Ok(Ok(())) => {}
        other => {
            panic!("upgrade must succeed once the other shared holder releases, got {other:?}")
        }
    }
}

#[test]
fn release_all_wakes_every_waiter() {
    let locks = Arc::new(LockManager::new());
    let r = rid(1);
    assert!(locks.lock(TxnId(1), r, LockMode::Exclusive).is_ok());

    let (tx_b, rx_b) = channel();
    let waiter_b = Arc::clone(&locks);
    thread::spawn(move || {
        let _ = tx_b.send(waiter_b.lock(TxnId(2), r, LockMode::Shared));
    });
    let (tx_c, rx_c) = channel();
    let waiter_c = Arc::clone(&locks);
    thread::spawn(move || {
        let _ = tx_c.send(waiter_c.lock(TxnId(3), r, LockMode::Shared));
    });

    match rx_b.recv_timeout(TIMEOUT) {
        Err(RecvTimeoutError::Timeout) => {}
        other => panic!("waiter B must block behind the exclusive holder, got {other:?}"),
    }
    match rx_c.recv_timeout(TIMEOUT) {
        Err(RecvTimeoutError::Timeout) => {}
        other => panic!("waiter C must block behind the exclusive holder, got {other:?}"),
    }

    locks.release_all(TxnId(1));

    match rx_b.recv_timeout(TIMEOUT) {
        Ok(Ok(())) => {}
        other => panic!("waiter B must be woken by release_all, got {other:?}"),
    }
    match rx_c.recv_timeout(TIMEOUT) {
        Ok(Ok(())) => {}
        other => panic!("waiter C must be woken by release_all, got {other:?}"),
    }
}

#[test]
fn opposite_order_locking_produces_a_deadlock_victim_for_exactly_one_side() {
    let locks = Arc::new(LockManager::new());
    let rid_a = rid(1);
    let rid_b = rid(2);

    assert!(locks.lock(TxnId(1), rid_a, LockMode::Exclusive).is_ok());
    assert!(locks.lock(TxnId(2), rid_b, LockMode::Exclusive).is_ok());

    let (tx1, rx1) = channel();
    let locks1 = Arc::clone(&locks);
    thread::spawn(move || {
        let _ = tx1.send(locks1.lock(TxnId(1), rid_b, LockMode::Exclusive));
    });

    let (tx2, rx2) = channel();
    let locks2 = Arc::clone(&locks);
    thread::spawn(move || {
        let _ = tx2.send(locks2.lock(TxnId(2), rid_a, LockMode::Exclusive));
    });

    let outcome1 = rx1.recv_timeout(TIMEOUT);
    let outcome2 = rx2.recv_timeout(TIMEOUT);
    let debug = format!("{outcome1:?} / {outcome2:?}");

    let victim = match (outcome1, outcome2) {
        (Ok(Err(TxnError::DeadlockVictim(id))), Err(RecvTimeoutError::Timeout)) => TxnId(id),
        (Err(RecvTimeoutError::Timeout), Ok(Err(TxnError::DeadlockVictim(id)))) => TxnId(id),
        _ => panic!(
            "expected exactly one side to fail with a deadlock and the other to still be \
             blocked, got {debug}"
        ),
    };

    locks.release_all(victim);

    let survivor_rx = if victim == TxnId(1) { &rx2 } else { &rx1 };
    match survivor_rx.recv_timeout(TIMEOUT) {
        Ok(Ok(())) => {}
        other => {
            panic!("the surviving side must be granted once the victim releases, got {other:?}")
        }
    }
}

#[test]
fn a_released_transaction_holds_nothing() {
    let locks = LockManager::new();
    let r = rid(1);
    assert!(locks.lock(TxnId(1), r, LockMode::Exclusive).is_ok());
    locks.release_all(TxnId(1));

    assert!(
        locks.lock(TxnId(2), r, LockMode::Exclusive).is_ok(),
        "a resource must be immediately available once its only holder released everything"
    );

    match locks.lock(TxnId(1), rid(2), LockMode::Shared) {
        Err(TxnError::LockAfterUnlock(1)) => {}
        other => {
            panic!("a released transaction must not be able to acquire new locks, got {other:?}")
        }
    }
}
