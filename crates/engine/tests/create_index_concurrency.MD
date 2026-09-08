# create_index_concurrency

Two-session checks that `CREATE INDEX` is serialized against concurrent
writers of the table it indexes, the M10.2 property P-62 found missing:
the statement takes an exclusive table lock through its own transaction
before it reads a single row, and publishes the index into the catalog
only once the backfill is finished
(`docs/adr/0017-create-index-is-a-locked-writer.md`). Single-session
`CREATE INDEX` correctness lives in `index_scan.MD`; this file is only
about what two sessions do to each other.

## Key Components

- `sorted_ints(rows) -> Vec<i32>` - flattens `rows_of`'s
  `Vec<Vec<Value>>` for a single `INTEGER` column into a sorted
  `Vec<i32>`, so an index scan's rows and a sequential scan's rows can be
  compared as sets without depending on either plan's arrival order.
- `create_index_waits_for_an_uncommitted_insert_and_never_indexes_it` -
  session A runs `BEGIN; INSERT INTO t VALUES (99);` and holds it; session
  B runs `CREATE INDEX` on a worker thread. B must still be blocked after
  `BLOCKED_WINDOW`, which is the assertion that it takes the lock rather
  than reading A's uncommitted row - the negative is safe to time-bound
  because a correctly locked B can never finish before A ends, whatever
  the machine's speed. A then rolls back, B completes, and `WHERE a = 99`
  returns nothing: had B indexed the dirty row, the entry would outlive
  the undone heap slot and the query would either return it or fail with
  `CorruptTuple`.
- `an_index_built_beside_a_concurrent_insert_holds_every_committed_row` -
  one session inserts `CONCURRENT_ROWS` autocommit rows while another
  builds the index over the same table. Whatever order the lock is granted
  in, the index scan (`WHERE a >= 0`) and the sequential scan
  (`SELECT a FROM t`) must return the same set: a row committed before the
  lock is granted is found by the backfill, and one inserted after is
  maintained by `InsertExecutor`, which cannot read a stale index list
  because it takes the same table lock first.

## Usage Example

```rust
let db = Database::open(DbConfig::new(dir.path().join("test.db")))?;
let mut writer = db.connect()?;
writer.execute("BEGIN")?;
writer.execute("INSERT INTO t VALUES (99)")?;

let mut builder = db.connect()?;
let build = thread::spawn(move || builder.execute("CREATE INDEX idx_t_a ON t (a)"));
// build is blocked here until:
writer.execute("ROLLBACK")?;
```

Run as part of `cargo test --workspace`, or just this file with
`cargo test -p engine --test create_index_concurrency`. Both tests bound
every wait with a timeout, so a regression that deadlocks fails rather
than hanging the suite.
