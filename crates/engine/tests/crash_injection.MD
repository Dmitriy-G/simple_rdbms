# crash_injection

The crash-injection harness: for a fixed, deterministic workload of SQL
statements, and for every possible point a write could fail (`1..=K`,
where `K` is the total number of writes the workload performs), opens a
fresh database on a `FaultyDevice`/`FaultySegmentStore` armed to fail at
write `n`, runs the workload until it dies, reopens the same files on a
real device (which runs `storage::recovery::recover` as part of
`Database::open`), and checks the result is consistent with *some* prefix
of the workload's statements: build a reference database by replaying
exactly the statements that were acknowledged before the fault struck
(against a fault-free device), and require the recovered database's
observable state - its set of tables and every row in each - to match it
exactly. The write half (`Database::open_with_devices`) and the recovery
half (`Database::open`) must open the identical on-disk WAL layout for
this comparison to mean anything, which is why the WAL side is a
`storage::wal::SegmentStore` here rather than a single `BlockDevice`: both
halves speak the same numbered segment family production actually writes
(`docs/adr/0011-segmented-write-ahead-log.md`).

This is the M7 deliverable per `../../../.claude/task.MD`: "this harness will find more
bugs than every other test in the repo combined."

### Durability models

Every sweep in this file runs under every one of the four
`storage::block_device::DurabilityModel` compositions:

- `write_is_durable()` (the harness's original model): a completed
  `write_at`/`set_len` is durable the instant it returns, and the fault
  only ever loses the one call it interrupted. This covers a crash that
  lands between two syscalls - e.g. the process dying between writing the
  WAL and writing a page - which is the failure class M7-M11's fixes
  target.
- `requires_sync()`: a completed `write_at`/`set_len` only becomes
  durable once a later `sync_all` succeeds; anything still pending when
  the fault fires is discarded. This additionally covers a crash or power
  loss that lands *after* a write() call returns but *before* the next
  fsync() - data sitting in the OS page cache, never flushed to the
  platter. Real systems lose writes this way constantly;
  `write_is_durable()` alone can never exercise it, because nothing
  before `requires_sync()` existed for it to be tested by.
- `torn_write()`: the tripped `write_at` call itself lands only a
  seeded-random subset of its 512-byte sectors on the inner device before
  reporting its error - a single `write_at` interrupted mid-sector, which
  neither of the models above can produce. This is the M12 failure class:
  a checksum (part 1) detects it, and the double-write buffer (part 2) is
  what lets recovery repair it instead of just reporting it. Applied
  uniformly to all three files (data, WAL, double-write buffer) like the
  other models, which also exercises a torn WAL record (handled by its
  own per-record CRC, independent of this file) and a torn double-write
  buffer slot or header (handled by `recovery::recover_double_write`
  treating a bad copy as "skip it," not just a torn real-file page).
- `torn_write_requires_sync()`: `torn_write()` and `requires_sync()`
  composed - the tripped call tears *and* everything else still needs a
  `sync_all` to be durable, so whatever was pending when the fault fired
  is lost outright rather than merely absent from that one call. This is
  what an actual power failure does, and it is the combination the
  double-write buffer exists to survive: the other three models each
  leave one of tearing or lost-unsynced-writes out, so none of them on
  its own tests the double-write buffer against the failure mode it was
  built for.

None of the four models cover a disk that lies about having synced (some
consumer SSDs and virtualized/cloud block devices under certain
configurations), or media failure large enough to fool CRC-32 in both a
page and its double-write copy at once. Those are outside what fault
injection at the `write_at`/`sync_all` call boundary can express at all.

## Key Components

`DeviceTriple`, `open_file`, and `faulty_devices` moved to the shared
`test-support` crate (`crates/test-support/src/devices.MD`) - this file's
own copy used to be byte-for-byte identical to
`storage/tests/btree_crash_injection.rs`'s. So did the sweep algorithm
itself: `run_until_crash` and `assert_workload_is_crash_safe` are now
`test_support::CrashWorkload`/`test_support::assert_workload_is_crash_safe`
(`crates/test-support/src/crash.MD`), generic over a trait this file
implements for its own SQL-driven workload rather than owning a private
copy of the sweep. What's left here is everything genuinely specific to
driving SQL through an `engine::Database`:

- `TableRows` - one table's name alongside every row in it, in the order
  produced; `SqlWorkload::State`.
- `HarnessConfig` - the WAL segment size, checkpoint byte threshold, and
  buffer pool size a given sweep opens its databases with; `DEFAULT`
  matches `common::DbConfig`'s own defaults, `SEGMENT_ROLLING` shrinks all
  three so a short workload still rolls and truncates a WAL segment.
- `config(dir, harness) -> DbConfig` - `test_support::db_config(dir)` with
  `harness`'s two `DbConfig` fields overlaid via struct update.
- `observable_state(db) -> Result<Vec<TableRows>, _>` - every table's name
  (sorted) and every row in each, in the order produced - the "observable
  state" two databases are compared on.
- `SqlWorkload<'a>` - `statements: &'a [String]`, `harness: HarnessConfig`;
  the `CrashWorkload` implementor driving every single-generation sweep
  below. `open` calls `Database::open_with_devices`. `drive` is the old
  `run_until_crash` body verbatim: `acked` (every statement that returned
  `Ok`) is not the same as "safely committed" - a `BEGIN` and the inserts
  after it can each individually return `Ok` while the fault then kills
  the process before the matching `COMMIT` ever runs, in which case none
  of them should be in the reference prefix, so the returned safe prefix
  only advances at a point nothing is left open (after an autocommit
  statement, or after a `COMMIT`/`ROLLBACK` that actually returned `Ok`).
  `expected_state` replays `statements[..safe_prefix]` into a fresh
  `Database` in its own temp dir and snapshots it; `observed_state` just
  snapshots the handle it's given (`safe_prefix` unused - a SQL database's
  observable state needs no prefix argument to compute, unlike the B+tree
  workload's, which uses it to decide whether a missing root pointer is
  expected or a real recovery bug). `test_support::assert_workload_is_crash_safe`'s
  fault-free first phase only requires the shared write counter to be
  positive, not that `drive` returns `statements.len()` - `mid_transaction_kill`
  deliberately ends mid-transaction, so every statement can return `Ok`
  while the safe prefix still falls short of the full statement count;
  see `crates/test-support/src/crash.MD` for the bug this shape once
  caused when the sweep assumed the two were equal.
- `many_small_inserts() -> Vec<String>` - many tiny single-column inserts
  into one table.
- `page_boundary_inserts() -> Vec<String>` - wide rows, enough of them to
  force at least one heap page allocation mid-workload (exercising
  `AllocPage`'s redo alongside `Update`'s).
- `create_table_then_inserts() -> Vec<String>` - a `CREATE TABLE`
  immediately followed by a handful of inserts - short enough that a
  large fraction of the crash-injection sweep's fail points land inside
  the `CREATE TABLE`'s own catalog-heap write.
- `interleaved_allocation_and_catalog_writes() -> Vec<String>` - two
  tables' `CREATE TABLE`s (catalog writes) and inserts (heap page
  allocation) interleaved, rather than one table fully built before the
  next starts.
- `mid_transaction_kill() -> Vec<String>` - a committed explicit
  transaction followed by one that's deliberately left open (its own
  trailing `BEGIN`/`INSERT` never reach a `COMMIT` within the fixed
  workload). Every fail point at or after that trailing `BEGIN` must
  recover to "row 3 doesn't exist" - this is the M8 "kill mid-
  transaction, before COMMIT, leaves no trace of it" case, swept across
  every write point rather than checked at just one.
- `total_write_count_after(prefix, workload) -> Result<u64, _>` - like
  `total_write_count`, but for a workload meant to run as a second process
  generation on top of `prefix` having already run to completion and been
  cleanly closed: returns the write count for `workload` alone, with
  `prefix`'s own writes (and that clean close's) excluded.
- `assert_two_generation_workload_is_crash_safe(setup, gen1, gen2, model)
  -> Result<(), _>` - crosses two process generations, the one shape none
  of the sweeps above exercise: every prior sweep crashes once and then
  only ever reopens cleanly afterward, so a freshly opened
  `TransactionManager` never gets a chance to collide with an id a
  *previous* generation's crash left dangling - which is exactly the M9
  bug (see `../../../.claude/task.MD`) this harness extension targets: a transaction whose
  `Commit` reached disk but whose `End` didn't is a "phantom winner" that
  recovery correctly never undoes, but if a later generation's
  `TransactionManager` isn't seeded past its id, a brand new transaction
  can be assigned that same id - and, unless recovery resets that id's
  Active Transaction Table state on `Begin`, the new, uncommitted
  transaction inherits the phantom's `committed` flag and is never undone
  either. `setup` runs once, cleanly, before the sweep even starts, so
  both generations' own first explicit transaction lands on the same id
  under the pre-fix bug (every open's bootstrap transaction takes id `0`,
  and each generation's own first explicit `BEGIN` is otherwise the very
  next id handed out) - the collision this test needs actually happens,
  not just hypothetically could. `gen1` is swept across every possible
  crash point (`1..=K1`), since only some of them land exactly between
  its `COMMIT`'s flush and its `End` ever reaching disk - the precise
  "phantom winner" shape. `gen2` is not fault-injected: it is constructed
  to never issue its own `COMMIT`, so simply running it to completion and
  dropping the database without closing already leaves it open, exactly
  like a real process exit mid-transaction.
- `two_generation_setup() -> Vec<String>` - the table both generations act
  on, created once via a clean, fault-free open before the sweep starts.
- `two_generation_gen1() -> Vec<String>` - generation 1: a single explicit
  transaction committing one row, swept across every crash point -
  including, crucially, every point after its `COMMIT` record is durable
  but before its `End` is.
- `two_generation_gen2() -> Vec<String>` - generation 2: a second explicit
  transaction that never commits.
- The remaining `#[test]` functions each call
  `assert_two_generation_workload_is_crash_safe` or
  `assert_workload_is_crash_safe` with one of the workloads above crossed
  against one or more `DurabilityModel`s; test names encode which
  workload and which model. `a_second_generation_transaction_never_corrupts_the_first_at_every_crash_point_with_torn_writes`
  in particular exists because a torn write to the real data file mid-
  flush must be repaired by the double-write buffer before recovery ever
  gets to the transaction-id-reuse hazard this sweep exists to catch, not
  compound it.

## Usage Example

```rust
#[test]
fn many_small_inserts_survive_a_crash_at_every_write() -> Result<(), Box<dyn Error>> {
    assert_workload_is_crash_safe(&many_small_inserts(), DurabilityModel::write_is_durable())
}
```

Run as part of `cargo test --workspace`; each sweep opens and reopens
dozens of temp-dir-backed databases per test (one per fail point times
every `DurabilityModel` composition), so this file is the slowest in the
suite but is deliberately the one most likely to catch a durability
regression before it reaches a real crash.
