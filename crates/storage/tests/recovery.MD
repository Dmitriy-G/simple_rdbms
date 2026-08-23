# recovery

`storage::recovery`: Analysis/Redo/Undo exercised directly against
hand-built logs, without any transaction-manager or catalog involved
(recovery doesn't know about either). Each test opens a `BufferPool`,
does some writes and explicit log bookkeeping to set up a scenario, drops
it without a clean shutdown (simulating a crash), then reopens a fresh
`BufferPool` over the same files and calls `recovery::recover`.

## Key Components

- `open_pool(dir, pool_size)` - opens a `BufferPool` over temp-dir files
  at `dir`.
- `redo_replays_committed_writes_that_never_reached_disk` - writes and
  commits a page, flushes the log but deliberately skips `flush_all`/
  `sync`: the commit is durable in the log, but the page itself never
  reached disk - exactly what no-force allows, and exactly what a crash
  right here would leave behind. After reopening and recovering, the
  page's bytes must be present.
- `undo_rolls_back_an_uncommitted_transactions_writes` - txn 1 commits a
  write; txn 2 overwrites the same bytes and even reaches disk (steal),
  but never commits. Recovery must undo txn 2's write, leaving txn 1's.
- `recovery_is_idempotent_when_run_twice` - calling `recovery::recover`
  twice on the same pool must leave identical page bytes both times -
  redo must be idempotent.
- `undo_resumes_from_a_partially_undone_transaction_without_redoing_a_completed_step` -
  simulates a first recovery attempt that undid exactly the second of
  txn 2's two updates and then crashed again before writing Abort/End, by
  manually appending the compensating `Clr` and stamping the page. A
  second recovery must finish the undo without re-undoing the
  already-undone second update - asserted both by the final page bytes
  and by counting `Clr` records for txn 2 (must be exactly 2: one from
  the simulated first attempt, one from resuming, never a third
  re-undoing the same step).
- `analysis_starts_from_the_checkpoint_and_still_finds_a_loser_that_began_before_it` -
  txn 2's first update happens before a checkpoint that captures it as
  still active; a second update happens after, and txn 2 never commits.
  Recovery must still undo both updates even though Analysis started
  scanning only from the checkpoint's LSN. Records the checkpoint's own
  LSN in the header via a dedicated committed transaction (`TxnId(9)`),
  matching `txn::write_checkpoint`'s own use of a dedicated header
  transaction.
- `begin_resets_stale_committed_state_left_by_a_reused_txn_id` -
  generation 1: `Begin(5)`, `Update(5)`, `Commit(5)` - deliberately no
  `End`, exactly what a crash between the flushed `Commit` and the
  unflushed `End` would leave behind. Generation 2 reuses id 5 - exactly
  what an unseeded `TransactionManager` would hand out next: `Begin(5)`,
  `Update(5)`, and nothing more. Without Analysis resetting the ATT entry
  on `Begin`, generation 2's write would inherit generation 1's stale
  `committed: true` and never get undone; this test asserts generation
  1's write survives and generation 2's is undone.
- `an_uncommitted_header_update_is_undone_like_any_other_page_mutation` -
  a transaction allocates a heap page and points the database header
  (`catalog_first_page`) at it, but crashes before committing. Per
  `task.MD`'s "route page 0 through the WAL," this must be undone exactly
  like any other page mutation - a raw, unlogged write (the bug this
  guards against) could never be rolled back this way, since only a
  properly WAL-tracked `Update` record lets `undo_transaction` find and
  revert it.
- `checkpoint_seeding_does_not_resurrect_a_transaction_the_scan_already_saw_end_for` -
  drives `LogManager` directly (the engine can't currently produce this
  exact interleaving) to construct a transaction that commits and ends
  *during* the checkpoint window - after `CheckpointBegin` is logged but
  before `CheckpointEnd` (carrying a snapshot captured slightly earlier,
  and so stale by the time it's logged) is. The checkpoint's own,
  already-stale snapshot must not resurrect that transaction as still
  active and get its committed write wrongly undone.

## Usage Example

```rust
let pool = open_pool(dir.path(), 8)?;
let (page_id, mut guard) = pool.new_page(TxnId(1))?;
guard.write(TxnId(1), 16, b"hello!")?;
drop(guard);
let commit_lsn = pool.append_log(TxnId(1), LogRecordKind::Commit)?;
pool.flush_log(commit_lsn)?;
drop(pool); // simulates a crash: no flush_all, no clean shutdown

let pool = open_pool(dir.path(), 8)?;
recovery::recover(&pool)?;
```

Run via `cargo test -p storage --test recovery`, driving
`storage::recovery::recover`/`undo_transaction` directly against
hand-assembled log scenarios that would be awkward or impossible to
reproduce through the full SQL engine - the crash-injection sweep in
`crates/engine/tests/crash_injection.rs` covers the same machinery from
the opposite direction, via real workloads and random fail points.
