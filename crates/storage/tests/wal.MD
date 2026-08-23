# wal

`LogManager` behavior: a mix of record kinds survives an append/flush/
reopen round trip with `prev_lsn` chained correctly per transaction; a
log file truncated mid-record (a crash mid-append) iterates cleanly up to
the last intact record instead of erroring; a single flipped byte fails
CRC at exactly the record it corrupted and stops there; and the
write-ahead invariant itself - no page ever reaches disk before the log
record describing it is durable - holds across a real, eviction-heavy
workload.

## Key Components

- `round_trip_mixed_record_kinds_survives_reopen` - appends one of each
  `LogRecordKind` variant across two transactions, flushes, reopens, and
  checks every record's LSN/`txn_id`/`kind` matches, and that `prev_lsn`
  chains each transaction's own records together independent of what
  other transactions interleaved between them.
- `truncated_mid_record_iterates_cleanly_up_to_last_intact_record` -
  appends `Begin`+`Commit`, flushes, then lops 3 bytes off the file's end
  (shorter than a complete record) to simulate a crash partway through
  appending. `iter_from` must yield only the intact `Begin`, not error.
- `flipped_byte_fails_crc_at_exactly_that_record_and_stops` - flips one
  byte inside the second of three records (leaving the first untouched)
  and checks `iter_from` yields only that untouched first record.
- `read_at_returns_each_records_own_lsn_before_and_after_the_durable_boundary` -
  `LogManager::read_at` must return the exact record at a given LSN
  whether that record is already durable on disk or still sitting
  unflushed in the in-memory buffer - the two branches `read_at` picks
  between internally.
- `offset_lsns_survive_reopen` - LSNs are byte offsets into the log
  stream now, so the next one assigned after a reopen must equal the
  file's length - not `highest_lsn + 1`, which would only make sense for
  a sequential counter.
- `wal_ordering_holds_across_an_eviction_heavy_workload` - the invariant
  `BufferPool::flush_pages` and M7's recovery both depend on: a page
  never reaches disk before the log record describing its most recent
  change is durable. Drives a 3-frame pool against 200 inserted tuples to
  force repeated eviction under pressure, so `write_observations`
  captures many real disk writes to check this against, not just the
  trivial case of a pool that never evicts.

## Usage Example

```rust
let mut log = LogManager::open("test.wal")?;
let lsn = log.append(LogRecord { txn_id: TxnId(1), kind: LogRecordKind::Begin })?;
log.flush(lsn)?;
let records: Vec<_> = log.iter_from(Lsn(0))?.collect();
```

Run via `cargo test -p storage --test wal`, driving
`storage::wal::LogManager` directly against real temp-dir files and raw
byte-level file surgery to simulate torn/corrupted writes - complementary
to the crash-injection sweep in `crates/engine/tests/crash_injection.rs`,
which exercises the same torn-write shapes through `FaultyDevice` and a
real workload instead of hand-editing bytes.
