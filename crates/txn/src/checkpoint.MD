# checkpoint

Writes a fuzzy checkpoint: a `CheckpointBegin` record, the current Active
Transaction Table and Dirty Page Table snapshots, and a `CheckpointEnd`
record carrying both. Records the `CheckpointBegin` LSN in the database
header so a future recovery knows where to start scanning. Deliberately
does *not* flush data pages or block any other activity — a fuzzy
checkpoint is exactly "log a snapshot," not a quiesce point.

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
  logging section calls out at `info` by name.

## Usage Example

```rust
use txn::write_checkpoint;

let begin_lsn = write_checkpoint(&pool, &mut txn_manager)?;
```

`engine::Database` calls `write_checkpoint` once at startup and again
whenever `buffer_pool.log_bytes_appended()` has grown past
`DbConfig::checkpoint_byte_threshold` since the last one, bounding how
much of the log a future recovery's Analysis pass has to scan.
