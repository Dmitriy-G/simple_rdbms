# recovery

ARIES crash recovery: Analysis, Redo, and Undo. `recover` is meant to be
called once, on every `Database::open`, before the catalog is loaded - the
log and the pages it describes must be back in a consistent state before
anything above `storage` starts trusting page contents. It never touches
the catalog, transaction-manager, or lock-manager concepts (this crate
isn't allowed to depend on `txn` anyway - see the workspace's crate
dependency-edge rules in `CLAUDE.md`), so it is phrased entirely in terms
of `BufferPool`/`LogManager`/`DiskManager`. `undo_transaction` is
deliberately `pub`: it is the one piece of this module a *runtime* abort
(not just crash recovery) also needs, so `txn::TransactionManager::abort`
calls it directly rather than reimplementing undo.

## Key Components

- `recover_double_write(disk, dwb) -> Result<(), StorageError>` - repairs
  a real-file page torn by a crash mid-write, using an in-flight
  double-write buffer batch, if there is one. Must run before `recover`
  (and before any `BufferPool` exists to run it against): ARIES's Redo
  pass trusts every page it reads to be otherwise intact, so a page torn
  by a crash must be restored *first* - this is what lets a
  `ChecksumMismatch` surviving to `recover`'s own Redo pass stay a hard,
  unrecoverable error (real media corruption) rather than something Redo
  itself has to work around. For each `(page_id, slot_crc)` entry the
  double-write buffer's header names: reads that slot's raw bytes and
  recomputes their CRC-32, comparing it against the entry's recorded
  `slot_crc` rather than the slot's own internal page checksum
  (`page::checksum_ok`) - `slot_crc` is the one signal that actually ties
  a slot's *exact* on-disk bytes to the moment `write_batch` wrote them,
  where `checksum_ok` only proves the bytes are *some* internally
  consistent page (or, deliberately, an all-zero one - see
  `dwb.MD`'s `checksum_ok` cross-reference - which is exactly the
  weaker property that let a torn or mismatched slot still look "good
  enough" to restore from before this check existed). A `slot_crc`
  mismatch has two possible causes, told apart by checking whether the
  actual bytes' CRC matches *any other* entry in this same batch's
  header: if it matches none, the crash landed while the copy itself was
  still being written (protocol steps 1-2) and the real page was never
  touched by this batch, so this slot is skipped - nothing to restore it
  from. If it matches a *different* entry's recorded `slot_crc`, this
  slot is holding a different page's content than its own header entry
  claims - the exact confusion a batch's slots being addressed by
  position rather than page id makes possible (`buffer.MD`'s poisoning
  paragraph walks through how, absent `flush_sequence`, two overlapping
  flushes used to produce exactly this state) - and using it would mean
  writing the wrong page's bytes over the real page it's being restored
  into; this returns `StorageError::DoubleWriteRestoreFailed` naming the
  page immediately, refusing the whole recovery pass rather than silently
  either skipping or misapplying it. A `slot_crc` match that turns out to
  be an all-zero slot is also skipped, never restored from, regardless of
  whether it matches its recorded crc: an honest, non-corrupted all-zero
  slot can occur (a batch entry position never actually written this
  session), and a freshly allocated page being legitimately all zero is
  exactly why `page::checksum_ok`'s own all-zero exemption exists for
  *that* job - but here, "safe to overwrite a live page with" is a
  strictly stronger claim than "internally consistent," and an
  all-zero slot never gets to make it. Once a slot's `slot_crc` matches
  its own entry and it isn't all-zero, it's safe to compare against the
  real page: if the real page's own checksum fails, it was torn by this
  batch's real-file write (step 4) and is restored from the copy; if it
  verifies, it is left alone - either this batch never reached it or
  reached it completely, and both are consistent states Redo can work
  from unmodified. A real page that isn't durably extended to *at all*
  yet (`PageNotFound`) is treated the same as "never reached it": this
  batch's own real-file write (step 4) never got that far, but the
  page's `AllocPage` record is already durable in the WAL (it could not
  have been dirtied otherwise), so ordinary ARIES redo will reconstruct
  it from an all-zero start the same way it already does for any page
  whose first-ever write never reached disk - restoring it here would be
  redundant at best, and, if a later entry in this same batch happens to
  extend the file first (zero-filling this page's still-untouched region
  as a side effect), indistinguishable from a legitimately empty page,
  which is exactly the state redo expects to start from anyway. The batch
  is only ever retired (`dwb.clear_batch()`) once every page this pass
  actually restored has been read back and reverified against its own
  checksum - a second crash landing on top of this pass's own restore
  writes is possible too, and a batch discarded before that is ruled out
  would take the only remaining intact copy down with it; if any restored
  page still fails its checksum, this returns
  `StorageError::DoubleWriteRestoreFailed` naming it, leaving the batch in
  place for a later attempt to retry. Logs a `warn!` naming how many pages
  it actually restored, if any - a non-empty restore means the previous
  shutdown was unclean, which an operator watching logs wants to know even
  though this function itself recovered from it cleanly - and increments
  `dwb_pages_restored_total` (a `metrics` counter - see
  `docs/ROADMAP.md`'s M13 entry) by that same count, so "was the last
  shutdown unclean" is answerable from a dashboard, not just a log line.
- `recover(pool) -> Result<Option<TxnId>, StorageError>` -
  `#[tracing::instrument(skip_all)]`. Runs Analysis,
  Redo, and Undo against `pool`'s write-ahead log, leaving every page
  consistent with the log's most recent durable state and every
  transaction that never committed fully undone. Idempotent: safe to call
  on a log that describes a database already in a consistent state (the
  common case), and safe to call again on a log left behind by a crash
  *during* a previous recovery attempt. Returns the highest `TxnId`
  observed anywhere in the log (excluding `CHECKPOINT_TXN`), or `None` if
  the log has never recorded a real transaction; `Database::open` threads
  this into `TransactionManager::new` so a freshly seeded id counter can
  never hand out an id the log has already used - which matters even for
  an id whose transaction is long since committed and ended, since a
  reused id's new `Begin` would chain its `prev_lsn` right onto that
  unrelated transaction's own log records. Analysis seeds its ATT (maps a
  transaction to its most recent LSN and whether it has committed) and DPT
  (maps a page to the LSN of the record that first dirtied it since its
  last flush) from the checkpoint's own `CheckpointEnd` snapshot *before*
  the forward scan runs at all - not merged in via `or_insert` at the
  moment the scan happens to reach `CheckpointEnd`, which is what the old
  code did. That mattered: a fuzzy checkpoint's snapshot is captured near
  `CheckpointBegin`'s time, but `CheckpointEnd` (carrying it) is only
  logged later, so any record in between - say, a transaction's `End` -
  gets processed by the scan *before* the snapshot that predates it. If
  that snapshot still shows the same transaction active and gets merged
  in with `or_insert`, it resurrects an entry the scan just correctly
  removed - undoing already-committed work. Seeding first and letting
  every subsequent record overwrite unconditionally fixes this: whichever
  is more recent always wins, regardless of which one the loop happens to
  visit first. A `Begin` record always resets the ATT entry for its
  `txn_id` rather than merging into whatever was there, since any prior
  entry belongs to a different transaction (an earlier generation reusing
  the id) - in particular, a "phantom winner" left behind by a crash
  between a `Commit` and its `End` must not make a brand new,
  never-committed transaction of the same id look committed too. Redo
  repeats history from the oldest change that might still be missing from
  disk (the DPT's minimum recovery LSN), replaying every logged
  `Update`/`Clr`/`AllocPage` - including losers' own actions, which is
  exactly why Undo runs after, not instead of, this pass. Recovery leaves
  a durable, checkpoint-able database behind rather than relying on later
  no-force flushes to eventually catch up: it calls `flush_log_all`,
  `flush_all`, and `sync` before returning. Emits a single `info!`
  "recovery complete" summary just before returning: `winners` (committed
  transactions the Analysis pass saw), `losers` (transactions Undo rolled
  back), `records_scanned` (the Analysis pass's own forward-scan record
  count), `redo_replayed` (how many Redo actually applied, as opposed to
  skipped because the page's `page_lsn` already covered them), and
  `duration_ms` - the line an operator reads after a crash to know what
  recovery actually did, not just that it finished. Also records the same
  duration into the `recovery_duration_seconds` histogram and adds
  `loser_count` to `recovery_losers_total` (both `metrics` instruments,
  per `docs/ROADMAP.md`'s M13 entry) - the dashboard-queryable form of the
  same two facts.
- `undo_transaction(pool, txn_id, from) -> Result<(), StorageError>` -
  undoes `txn_id`'s writes, walking its log chain backward starting from
  `from` (that transaction's most recent LSN). For each `Update` found,
  logs a `Clr` carrying the before-image and applies it, then continues
  from the `Update`'s own `prev_lsn`; a `Clr` is never itself undone -
  walking through one just continues from its `undo_next_lsn`. Once the
  chain is exhausted, logs `Abort` then `End`. Shared by two callers:
  crash recovery's Undo pass (`recover`) and
  `txn::TransactionManager::abort`'s ordinary runtime abort - there is
  exactly one undo implementation, not two. A transaction's undo chain
  never extends past its own `Begin`, regardless of what `prev_lsn` that
  record carries: `LogManager` chains `prev_lsn` purely by `TxnId`, so if
  this id has ever been reused, `Begin`'s own `prev_lsn` points at an
  unrelated, earlier generation's last record for the same id - walking
  past it here would cascade into undoing that generation's
  already-committed writes too. Resuming after a crash *during* undo
  works for free: on a second recovery attempt, Analysis finds the loser
  still active with `last_lsn` now pointing at the last `Clr` that made
  it to disk, and walking from there immediately follows that `Clr`'s
  `undo_next_lsn` rather than re-undoing the `Update` it already
  compensated for. Redo (which always runs first) independently
  reapplies any `Clr` whose own page write didn't survive the second
  crash, which is why Redo must replay losers' actions - including their
  `Clr`s - and must be idempotent.

## Usage Example

```rust
use storage::recovery;

recovery::recover_double_write(&mut disk, &mut dwb)?;
let pool = storage::buffer::BufferPool::new(disk, dwb, log, 64, replacer);
let max_txn_id = recovery::recover(&pool)?;
```

`engine::Database::open` calls `recover_double_write` before any
`BufferPool` exists, then constructs the pool and calls `recover` on it
before loading the catalog - both run unconditionally on every open, not
just after an unclean shutdown, since they are cheap no-ops on an
already-consistent database. `txn::TransactionManager::abort` calls
`undo_transaction` directly for an ordinary runtime rollback, the same
routine crash recovery's own Undo pass uses.
