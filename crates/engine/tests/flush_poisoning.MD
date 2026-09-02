# flush_poisoning

Proves `engine::Database`'s response to `storage::buffer::BufferPool`
being poisoned (`buffer.MD`'s poisoning paragraph) is a single, immediate,
clearly-named failure rather than the silent degradation `../../../.claude/task.MD`'s P-3
diagnosed: a poisoned pool used to keep acknowledging statements that
touched no dirty-enough page, let `maybe_checkpoint` retry and warn once
per statement forever without ever advancing `bytes_at_last_checkpoint`,
and only actually fail once eviction pressure finally found no clean
frame left - by which point the write-ahead log had been growing
unboundedly the whole time. `database.MD`'s `Database::execute` and
`Database::maybe_checkpoint` entries describe the fix this test exercises
from outside `engine`, through the public `Database` API alone.

## Key Components

- `SETUP_STATEMENT` - `"CREATE TABLE t (a INTEGER)"`, the one statement
  both the probe run and the real run execute; a `CREATE TABLE` is enough
  to dirty and (via the forced checkpoint below) flush page 0, and needs
  no other setup.
- `open_file(path)` - private. Opens (creating if necessary) a real file
  for a device to wrap, matching `crates/engine/tests/crash_injection.rs`'s
  helper of the same name.
- `config(dir)` - private. A `DbConfig` with `checkpoint_byte_threshold: 1`
  so `Database::maybe_checkpoint` fires on the very first statement's
  autocommit, regardless of how little the log has actually grown -
  the default multi-megabyte threshold would never trip from one
  `CREATE TABLE`.
- `open_with_counted_data_device(dir, counter, fail_at)` - private. Opens
  a `Database` (`Database::open_with_devices`) whose data-file device is a
  `storage::block_device::FaultyDevice` whose failures share `counter`; the
  double-write buffer and write-ahead log devices are real, unfaulted
  files, since only the data file's write matters here and faulting the
  others would just make the pool's own DWB write fail instead of the
  real-page write the poisoning window actually needs
  (`buffer.MD`'s `flush_pages` entry).
- `total_wal_bytes(dir)` - private. Sums the size of every file in `dir`
  whose name starts with `test.db.wal` - the whole segment family
  (`docs/adr/0011-segmented-write-ahead-log.md`), not just the active
  segment - so a check that the WAL "did not grow" can't be fooled by
  growth landing in a newly-rolled segment instead of the one already open.
- `a_poisoned_pool_fails_the_next_statement_rather_than_degrading` - runs
  `SETUP_STATEMENT` twice, once in a throwaway `probe_dir` against a
  `FaultyDevice` armed with `fail_at: u64::MAX` (so it counts every
  `write_at`/`set_len` call the data device sees but never actually fails
  one), to learn exactly how many such calls this workload makes
  end-to-end - `Database::open_with_devices`'s own header write for a
  brand-new file, every page allocation's `set_len`, and, last, the one
  real `write_at` `txn::write_checkpoint`'s `flush_page(PageId(0))`
  performs once `set_last_checkpoint_lsn` has dirtied page 0. That last
  call is always the final tick this workload produces, since nothing
  after it in `write_checkpoint` touches the data device by `write_at` or
  `set_len` (`pool.sync()` calls `sync_all`, which `FaultyDevice` never
  faults). Arming a *second*, real run with `fail_at` set to one less than
  that total count therefore fails precisely that last call and nothing
  before it - `Database::open` itself, and every legitimate page
  allocation along the way, still succeed - which is what turns "some
  write during this workload fails" into the exact window
  `buffer::BufferPool::flush_pages`'s poisoning protects: a real page
  write failing after its double-write batch already synced. Asserts
  three things in order: `SETUP_STATEMENT` itself still returns `Ok`
  (`Database::maybe_checkpoint` swallows `write_checkpoint`'s error, so
  the poisoning is an invisible side effect of an otherwise-successful
  statement); the next statement, an `INSERT`, fails with exactly
  `common::Error::FlushPoisoned` rather than succeeding or failing with
  some other error; and `total_wal_bytes` is unchanged between those two
  points, since `Database::execute`'s poisoned check now runs before
  `execute_impl` ever begins a transaction or parses the statement.

## Usage Example

```rust
let probe_counter = Arc::new(AtomicU64::new(0));
let mut probe_db = open_with_counted_data_device(probe_dir, Arc::clone(&probe_counter), u64::MAX)?;
probe_db.execute(SETUP_STATEMENT)?;
let fail_at = probe_counter.load(Ordering::Relaxed) - 1;

let mut db = open_with_counted_data_device(dir, Arc::new(AtomicU64::new(0)), fail_at)?;
db.execute(SETUP_STATEMENT)?; // still Ok; the pool is now poisoned
assert!(matches!(db.execute("INSERT INTO t VALUES (1)"), Err(common::Error::FlushPoisoned)));
```

Run as part of `cargo test --workspace` like any other `crates/engine/tests`
file; unlike `crash_injection.rs` it drives exactly one, precisely-aimed
fault rather than sweeping every possible crash point, so it needs no
special CI treatment and finishes in milliseconds.
