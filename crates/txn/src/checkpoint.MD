# checkpoint

Writes a fuzzy checkpoint: a `CheckpointBegin` record, the current Active
Transaction Table and Dirty Page Table snapshots, and a `CheckpointEnd`
record carrying both. Records the `CheckpointBegin` LSN in the database
header so a future recovery knows where to start scanning. Deliberately
does *not* flush data pages or block any other activity — a fuzzy
checkpoint is exactly "log a snapshot," not a quiesce point. Also the only
call site that ever shrinks the write-ahead log: once the checkpoint
records are durable, it computes how far back the log can safely be
truncated and calls `BufferPool::truncate_log_below`
(`docs/adr/0011-segmented-write-ahead-log.md`).

Lives in `txn` rather than `storage` because it is the lowest crate
allowed to see both an Active Transaction Table (owned by
`TransactionManager`) and a Dirty Page Table (owned by `BufferPool`) —
`storage` cannot depend on `txn` per the workspace's crate dependency
rules.

**A checkpoint runs in two phases, and the split is the whole point of
the API.** `write_checkpoint_record` needs the `TransactionManager` and
touches no data page; `finish_checkpoint` flushes page 0, syncs the
device and truncates the log, and needs no `TransactionManager` at all.
The caller holds whatever lock guards its transaction manager across the
first and releases it before the second, so a checkpoint's page flush and
`fsync` never stall a session that only wants to begin a transaction.
`write_checkpoint` is the two calls in sequence, for a caller that has
exclusive access anyway (`crates/txn/tests/lifecycle.rs`) and is not
racing anyone. Running the whole thing under one lock is what
`docs/adr/0016-a-checkpoint-is-not-a-quiesce-point.md` was written to
stop: the sentence above about not blocking other activity was false for
as long as the engine did it, since `EngineShared.txn_manager` is the
mutex every statement needs to begin its transaction.

## Key Components

- `PendingCheckpoint` - the value that carries a checkpoint between its
  two phases: the `CheckpointBegin` LSN, the truncation bound computed
  from the state the first phase saw, and the `Instant` the checkpoint
  started so the duration histogram measures the whole of it and not just
  the second half. Its fields are private, so the only way to obtain one
  is `write_checkpoint_record` and the only thing to do with it is
  `finish_checkpoint`; `begin_lsn()` reads the LSN back for a caller that
  wants it before finishing. It is `#[must_use]`: dropping one silently
  skips the header flush and the truncation, which is not incorrect —
  recovery redoes the header write from the log — but does quietly waste
  the checkpoint.
- `write_checkpoint_record(pool, txn_manager) -> Result<PendingCheckpoint,
  TxnError>` - phase one, the part that needs the transaction manager.
  Appends `CheckpointBegin`, snapshots the Active Transaction Table and
  the Dirty Page Table, appends `CheckpointEnd` carrying both, flushes the
  log, then begins/writes/commits the header transaction described below
  and computes the truncation bound. Every I/O it does is to the *log*,
  which is what every ordinary commit already does under the same caller's
  lock; it never flushes a data page.
- `finish_checkpoint(pool, pending) -> Result<Lsn, TxnError>` - phase two,
  the part that must not hold a lock. Flushes page 0, syncs the device,
  truncates the log below the bound phase one computed, records the
  metrics and emits the `info!`. The bound stays valid across the gap
  between the phases: a transaction beginning after phase one gets a
  higher `Begin` LSN than the bound, and a page dirtied after phase one
  gets a recLSN later than every LSN phase one saw, so neither can move
  the minimum down. Two checkpoints overlapping here is safe for the same
  reason plus one more — `truncate_log_below` with the *smaller* bound
  after the larger one has already run removes nothing extra.
- `write_checkpoint(pool, txn_manager) -> Result<Lsn, TxnError>` - the two
  phases in sequence, for a caller holding exclusive access to the
  transaction manager for other reasons. The engine does **not** use it:
  `engine::EngineShared::checkpoint_and_flush` and `maybe_checkpoint` call
  the two halves so the lock can be dropped in between
  (`docs/adr/0016-a-checkpoint-is-not-a-quiesce-point.md`). Writes
  a fuzzy checkpoint against `pool`, using `txn_manager` for the Active
  Transaction Table snapshot, and returns the `CheckpointBegin` record's
  LSN. Recording the checkpoint's own LSN in the header (page 0) is an
  ordinary, logged page mutation like any other, so — per the storage
  engine's original design task's "route page 0 through the WAL" rule —
  it needs a real transaction of its own, committed immediately: a header
  write that could outlive a crash without ever being logged is exactly
  the bug that rule fixed. That header transaction is begun, written, and
  committed against `txn_manager` after the checkpoint records themselves
  are logged and flushed. Records `checkpoint_duration_seconds` (a
  `metrics` histogram) and sets `checkpoint_last_lsn` (a gauge, so it
  always reads the most recent value rather than accumulating) - see
  `docs/ROADMAP.md`'s M13 entry - then emits an `info!` naming `lsn` right
  before returning; checkpoint completion is one of the events CLAUDE.md's
  logging section calls out at `info` by name. Finally computes
  `min(the lowest recLSN in the Dirty Page Table, `txn_manager`'s
  `earliest_active_begin_lsn`, this checkpoint's own `begin_lsn`)` -
  whichever of those exist, since an empty Dirty Page Table or no active
  transactions simply drop out of the `min` - and calls
  `pool.truncate_log_below` with it. Using each active transaction's
  *earliest* LSN rather than its most recently appended one (the value
  `active_snapshot` reports, used only for the `CheckpointEnd` record
  itself) is deliberate: `wal.MD`'s truncation invariant is that nothing
  undo or redo can still reach may be deleted, and an active transaction's
  undo chain can reach all the way back to its own `Begin` record
  regardless of how many of its pages have since been flushed to disk.
  Before computing that bound, forces the header transaction's write to
  page 0 out to disk with `pool.flush_page(PageId(0))` followed by
  `pool.sync()`. Page 0 is normally one of the hottest pages in the
  database, but "normally" is not "always" — under enough eviction
  pressure from everything else a checkpoint touches, it can genuinely be
  evicted between checkpoints, which is exactly why `flush_page` treats a
  non-resident page as an already-durable `Ok(())` rather than
  `StorageError::PageNotFound` (`buffer.MD`): ordinary eviction must never
  turn into a checkpoint failure. Without this explicit flush call, the
  on-disk copy of `last_checkpoint_lsn` can still lag arbitrarily far
  behind the in-memory value — potentially all the way back to the
  previous checkpoint, or to nothing at all if page 0 has never been
  flushed. A crash before that stale on-disk LSN reaches disk, followed by
  truncation, must not be possible: recovery reads that LSN as its start
  point, and `truncate_log_below` deletes exactly the segments a stale
  start point would need. The order is what makes this safe — the header
  LSN is durable *before* `truncate_log_below` runs, never after.

  **Why truncation can never make `next_txn_id` regress.**
  `txn::TransactionManager::new` seeds its id counter from
  `storage::buffer::BufferPool::max_txn_id`, which - after segmentation -
  only sees whatever transaction ids still appear in the surviving log
  segments (`docs/adr/0011-segmented-write-ahead-log.md`); anything
  named only in a segment `truncate_log_below` has already deleted is
  invisible to it. Since `TxnId`s are handed out by a strictly increasing
  counter, this is safe only because every checkpoint's `header_txn` -
  the fresh, ordinary transaction this function begins and commits a few
  lines above to write `last_checkpoint_lsn` - is itself assigned the
  highest id in use *at that moment*, and its own `Begin`/`Update`/
  `Commit`/`End` records are logged at LSNs at or after this checkpoint's
  own `begin_lsn`, which is one of the three values `truncate_bound`
  takes the `min` of - so `header_txn`'s records can never fall below the
  bound this same checkpoint computes, and can never be part of what
  *this* call to `truncate_log_below` removes. Any transaction with a
  *higher* id must have begun after `header_txn` and therefore logs even
  later, so it survives for the same reason. The next checkpoint repeats
  this with its own, newer `header_txn`, so by induction the most recent
  checkpoint's header transaction is always a safe, present lower bound
  for `max_txn_id` on every future reopen - a reopened database's
  `next_txn_id` can undercount how many transactions actually ran, but it
  can never undercount past the highest id that ever needs remembering.
  If `set_last_checkpoint_lsn` ever stopped going through a fresh,
  ordinarily-logged transaction of its own - becoming an unlogged direct
  write, for instance, or reusing an existing transaction - this
  guarantee breaks silently: nothing would then guarantee any record
  naming the current maximum id survives truncation, a reopened database
  could hand out an id already used by a truncated-away transaction, and
  `storage::wal::LogManager`'s purely-by-`TxnId` `prev_lsn` chaining
  (`recovery.MD`'s `undo_transaction` entry) would splice the new
  generation's undo chain onto the old one's, silently corrupting rollback
  for both.

## Usage Example

```rust
use txn::{finish_checkpoint, write_checkpoint_record};

let pending = {
    let mut txn_manager = txn_manager_mutex.lock().unwrap();
    write_checkpoint_record(&pool, &mut txn_manager)?
};
let begin_lsn = finish_checkpoint(&pool, pending)?;
```

`engine::Database` checkpoints once at startup and again whenever
`buffer_pool.log_bytes_appended()` has grown past
`DbConfig::checkpoint_byte_threshold` since the last one, bounding how
much of the log a future recovery's Analysis pass has to scan. It always
drives the two phases separately, exactly as above, because the lock in
that snippet is the one every statement needs to begin a transaction. A
caller with no such lock — a test, or a single-threaded tool — can call
`write_checkpoint` instead and get the identical sequence.
