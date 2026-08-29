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

## Key Components

- `write_checkpoint(pool, txn_manager) -> Result<Lsn, TxnError>` - writes
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
  `pool.sync()`. Page 0 is the hottest page in the database and is
  effectively never evicted on its own, so without this call the on-disk
  copy of `last_checkpoint_lsn` can lag arbitrarily far behind the
  in-memory value — potentially all the way back to the previous
  checkpoint, or to nothing at all if page 0 has never been flushed. A
  crash before that stale on-disk LSN reaches disk, followed by
  truncation, must not be possible: recovery reads that LSN as its start
  point, and `truncate_log_below` deletes exactly the segments a stale
  start point would need. The order is what makes this safe — the header
  LSN is durable *before* `truncate_log_below` runs, never after.

## Usage Example

```rust
use txn::write_checkpoint;

let begin_lsn = write_checkpoint(&pool, &mut txn_manager)?;
```

`engine::Database` calls `write_checkpoint` once at startup and again
whenever `buffer_pool.log_bytes_appended()` has grown past
`DbConfig::checkpoint_byte_threshold` since the last one, bounding how
much of the log a future recovery's Analysis pass has to scan.
