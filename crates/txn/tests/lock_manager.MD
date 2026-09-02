# lock_manager

Exercises `LockManager` under real thread contention - proving it actually
blocks, wakes, upgrades, and detects deadlocks across threads, not just
that its methods type-check against a single caller (see `smoke.rs`).
Every test that expects blocking bounds its wait with `TIMEOUT` so a
regression that makes `LockManager` hang fails the test suite instead of
the CI job.

## Key Components

- `rid(slot) -> Rid` - private. Builds a `Rid` on a fixed `PageId(0)` for
  tests that only care about distinguishing slots.
- `TIMEOUT` - the bound (500ms) every blocking `recv_timeout` in this file
  uses, both to confirm a call is genuinely still blocked (times out) and
  to confirm a woken call completes (doesn't).
- `two_shared_holders_coexist` - two transactions both acquire `Shared` on
  the same `Rid` without either blocking.
- `table_locks_conflict_like_row_locks` - the same compatibility rules
  apply to `LockManager::lock_table`, on its own `TableId`-keyed resource
  space, separate from row locks.
- `exclusive_waits_for_shared_then_is_granted_after_release` - a second
  transaction's `Exclusive` request blocks behind a `Shared` holder, then
  is granted once `release_all` runs for the holder.
- `upgrade_succeeds_immediately_when_sole_holder` - a transaction that
  already holds `Shared` alone on a `Rid` upgrades to `Exclusive` without
  blocking.
- `upgrade_waits_behind_another_shared_holder_then_succeeds` - the same
  upgrade blocks while a second transaction also holds `Shared`, and
  completes once that second transaction releases.
- `release_all_wakes_every_waiter` - two transactions both blocked behind
  one `Exclusive` holder are both granted `Shared` once `release_all` runs
  for the holder, proving the wakeup isn't limited to one waiter.
- `opposite_order_locking_produces_a_deadlock_victim_for_exactly_one_side` -
  two transactions each hold one `Rid` and then request the other's, in
  opposite order; exactly one request fails with
  `TxnError::DeadlockVictim` while the other stays blocked, and releasing
  the victim's locks lets the survivor's blocked request complete.
- `a_released_transaction_holds_nothing` - after `release_all`, a second
  transaction can immediately acquire what the first held, and the first
  transaction's own `lock` call now fails with `TxnError::LockAfterUnlock`
  rather than silently reacquiring.

## Usage Example

```rust
#[test]
fn example_two_shared_locks_coexist() {
    let locks = txn::LockManager::new();
    let r = common::Rid::new(common::PageId(0), 0);
    assert!(locks.lock(common::TxnId(1), r, txn::LockMode::Shared).is_ok());
    assert!(locks.lock(common::TxnId(2), r, txn::LockMode::Shared).is_ok());
}
```

Run as part of `cargo test --workspace`; this is the crate's proof that
`LockManager`'s blocking, wakeup, upgrade, and deadlock-detection
behavior is correct under real OS threads, which `tests/smoke.rs` alone
(construction only) would leave unverified.
