# lock_manager_growth

Drives `engine::Database` through real SQL statements and asserts that
`txn::LockManager`'s `finished` set - the bookkeeping `release_all` uses
to reject a lock request arriving after a transaction has already ended -
stays bounded rather than retaining one `TxnId` per transaction for the
life of the process (`txn::lock_manager.MD`'s `prune_finished`).
`crates/txn/tests/lock_manager.rs` already proves `LockAfterUnlock` itself
against a bare `LockManager`; this file proves the pruning that keeps
`finished` from growing forever is actually wired all the way through
`engine::Database::execute`, autocommit included, the same way
`version_store_growth.rs` proves it for `txn::VersionStore`.

## Key Components

- `AUTOCOMMIT_ROW_COUNT` - private. `300`, matching
  `version_store_growth.rs`'s choice - large enough that a regression
  back to unbounded growth would be obvious.
- `AUTOCOMMIT_ROW_COUNT_WHILE_OPEN` - private. `20` autocommit statements
  run while a second session's transaction is still open, each one its
  own finished transaction with a `TxnId` above the open transaction's
  own - enough to make growth in `finished` visible while the watermark
  is held back.
- `finished_txn_set_stays_bounded_across_autocommit_statements_and_an_open_transaction` -
  inserts `AUTOCOMMIT_ROW_COUNT` rows, each its own autocommit statement,
  and asserts `Database::stats()?.finished_txn_count` is exactly `0` once
  the last one commits - each commit's own
  `TransactionManager::oldest_active_txn_id` watermark is the *next*
  id to be issued (nothing else is active), so `prune_finished` drops the
  just-inserted entry in the same call that added it. It then opens a
  second session, begins an explicit transaction (taking no locks yet),
  and runs `AUTOCOMMIT_ROW_COUNT_WHILE_OPEN` more autocommit statements
  from the first session - each with a `TxnId` above the open
  transaction's, so the watermark stays pinned at the open transaction's
  id and none of them can be pruned - and asserts `finished_txn_count` is
  greater than zero. Committing the open transaction moves the watermark
  past every id accumulated during that window, and the assertion that
  `finished_txn_count` drops back to `0` confirms that commit's own
  `prune_finished` call swept all of them, not just its own entry.

## Usage Example

```rust
use common::DbConfig;
use engine::Database;

let mut db = Database::open(DbConfig::new(path))?;
db.execute("CREATE TABLE t (a INTEGER)")?;
for i in 0..300 {
    db.execute(&format!("INSERT INTO t VALUES ({i})"))?;
}
assert_eq!(db.stats()?.finished_txn_count, 0);
```

Run as part of `cargo test --workspace`, or in isolation with
`cargo test -p engine --test lock_manager_growth`. Needs `Database::stats`,
gated `#[cfg(any(test, feature = "test-util"))]`: the same
self-referencing `engine` dev-dependency with `test-util` enabled that
makes `EngineStats::version_chains` visible to `version_store_growth.rs`
also makes `EngineStats::finished_txn_count` visible here.
