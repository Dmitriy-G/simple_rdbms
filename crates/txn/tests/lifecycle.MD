# lifecycle

`TransactionManager::begin`/`commit`/`abort` and `write_checkpoint`
against a real `BufferPool`, proving the wiring — not just that the types
construct (see `smoke.rs`).

## Key Components

- `open_pool(dir) -> Result<BufferPool, Box<dyn Error>>` - private. Builds
  a real, file-backed `BufferPool` (disk, WAL, and double-write buffer all
  rooted at `dir`).
- `begin_assigns_a_fresh_id_and_commit_removes_it_from_active` - two
  `begin` calls assign distinct ids; committing one removes only that one
  from the active set, leaving the other untouched.
- `abort_undoes_writes_via_the_shared_undo_routine` - writes a page,
  commits, then overwrites it under a second transaction and aborts;
  checks the page's bytes are restored to the committed before-image via
  `storage::recovery::undo_transaction`.
- `write_checkpoint_captures_active_transactions_and_dirty_pages` -
  begins a transaction, dirties a page, and checks `write_checkpoint`
  records the header's `last_checkpoint_lsn`, and that the resulting
  Dirty Page Table and Active Transaction Table snapshots contain the
  dirtied page and active transaction respectively.
- `commit_of_an_unknown_transaction_is_an_error` - committing a `TxnId`
  the manager never assigned returns an error rather than panicking.
- `recovery_seeds_the_next_id_past_every_id_the_log_has_ever_used` - logs
  a transaction directly (bypassing `TransactionManager`), reopens and
  runs `recovery::recover`, and checks a `TransactionManager` seeded from
  its `highest_seen` result hands out an id past the one already used in
  the log.

## Usage Example

```rust
#[test]
fn example_commits_a_transaction() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let pool = open_pool(dir.path())?;
    let mut manager = txn::TransactionManager::new(None);
    let txn = manager.begin(&pool, txn::IsolationLevel::ReadCommitted)?;
    manager.commit(txn, &pool)?;
    Ok(())
}
```

Run as part of `cargo test --workspace`; this is the crate's proof that
`TransactionManager` and `write_checkpoint` correctly drive the real
`storage` primitives (the WAL, the buffer pool's dirty page table, and
`storage::recovery`) rather than just holding in-memory state, which is
what `tests/smoke.rs` alone would leave unverified.
