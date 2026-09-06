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
- `a_transaction_begun_after_a_commit_has_a_strictly_greater_read_ts` -
  begins a transaction, reads its `read_ts`, commits it, begins a second
  transaction, and checks the second one's `read_ts` is strictly greater -
  proving `TransactionManager`'s shared timestamp counter actually
  advances across a commit rather than each `begin` reusing the same
  value.
- `a_committed_writers_version_is_visible_only_to_readers_begun_at_or_after_its_commit` -
  a transaction begun before a writer even starts stays unable to see a
  `Rid` the writer records through
  `TransactionManager::version_store().record_insert` both before and
  after the writer commits, while a transaction begun after the commit
  can - proving `commit` calls `version_store().commit_versions` with a
  `commit_ts` that lands after every reader that started earlier, and
  before every reader that starts later.
- `oldest_active_read_ts_tracks_the_active_set` - with nothing active,
  `oldest_active_read_ts` reports the next timestamp to be issued; with
  two transactions open it reports the older one's `read_ts`; once the
  older one commits it reports the survivor's `read_ts`; once the
  survivor commits too it is back to a fresh next-timestamp value, which
  must exceed every `read_ts` already handed out - pinning the watermark
  `VersionStore::prune` (subtask 3) is driven by.
- `an_aborted_writers_version_never_resurfaces_for_a_later_committed_writer` -
  a transaction records a version at a `Rid` and aborts, then a second
  transaction records and commits its own version at the *same* `Rid`; a
  reader begun before the second commit does not see anything there, and
  one begun after sees only the second writer's version - proving
  `abort` actually discards the first writer's version rather than
  leaving it to interfere with (or get confused for) a later one. The gap
  between the abort and that later `record_insert` is also invisible: the
  emptied chain `abort_versions` leaves behind in the map is not the same
  as "no chain for this `Rid`", so `VersionStore::is_visible` does not
  read it as visible to everyone. See `crates/txn/src/version_store.MD`'s
  note on `abort_versions`.
- `commit_with_no_other_transaction_active_prunes_the_chain_immediately` -
  a writer commits with nothing else active; `chain_count()` is `0`
  immediately afterward, since `TransactionManager::commit` calls
  `VersionStore::prune` against a watermark that already covers the fresh
  `commit_ts`.
- `committed_chain_survives_an_older_reader_and_is_pruned_once_that_reader_ends` -
  an older reader is active when the writer commits, so the watermark sits
  below `commit_ts` and the chain survives (`chain_count() == 1`); once
  that reader itself commits, the watermark reaches `commit_ts` and the
  next `prune` (run as part of the reader's own commit) drops it.
- `a_row_committed_before_a_reader_began_stays_visible_after_its_chain_is_pruned` -
  after a chain is pruned to nothing (nothing else was active at commit
  time), a transaction begun afterward still sees the row as visible via
  `VersionStore::is_visible` - proving pruning does not change any answer
  a reader is entitled to, only how the store represents it internally.
- `abort_with_no_other_transaction_active_prunes_the_emptied_chain_immediately` -
  the abort half of the first test above: a writer inserts and aborts with
  nothing else active, and `chain_count()` is `0` immediately afterward.
- `emptied_chain_survives_an_older_reader_and_is_pruned_once_that_reader_ends` -
  the abort half of the second test above: an older reader keeps the
  emptied chain alive across the abort, and it is only pruned once that
  reader itself leaves the active set.

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
