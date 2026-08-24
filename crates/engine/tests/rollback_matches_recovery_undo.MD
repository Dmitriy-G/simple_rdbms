# rollback_matches_recovery_undo

Proves `TransactionManager::abort` (what `ROLLBACK` calls) and
`storage::recovery::recover`'s own Undo pass really are one
implementation, not two that happen to agree on paper: undoing the same
transaction's writes via an explicit `ROLLBACK` and via a crash before
that `ROLLBACK` ever runs must leave byte-identical database files.

Neither scenario calls `close` (which would write a checkpoint, adding
bytes unrelated to undo itself and unique to whichever scenario happened
to call it) - each is just dropped, the same "no shutdown code ran"
starting point a real crash leaves behind. No fault injection is needed
here: scenario B's "crash" is simply never issuing the `ROLLBACK`
statement and dropping the database, which is exactly what a kill signal
would leave behind - `Database::open`'s recovery pass is then what has to
notice the still-active transaction and undo it.

## Key Components

- `config(dir) -> DbConfig` - builds a `DbConfig` at `dir.join("test.db")`.
- `rollback_and_recovery_undo_produce_byte_identical_files` - runs the
  same `CREATE TABLE`/`BEGIN`/two `INSERT`s workload twice. Scenario A
  explicitly issues `ROLLBACK` in-process, then drops the database
  without `close`. Scenario B never issues `ROLLBACK` at all - the
  database is dropped without `close` and without `ROLLBACK`, simulating
  a kill signal landing mid-transaction - and is then reopened once more
  purely to run recovery's Undo pass (via the exact same
  `storage::recovery::undo_transaction` `abort` calls `ROLLBACK` itself
  uses) before being dropped again. Asserts the two scenarios' database
  files are byte-for-byte identical.

## Usage Example

```rust
let dir_a = tempfile::tempdir()?;
{
    let mut db = Database::open(config(dir_a.path()))?;
    for stmt in workload_without_rollback {
        db.execute(stmt)?;
    }
    db.execute("ROLLBACK")?;
}
let bytes_a = std::fs::read(dir_a.path().join("test.db"))?;
```

Run as part of `cargo test --workspace`; this is the test that would
catch `ROLLBACK` and crash-recovery undo drifting apart - e.g. one
gaining a code path or byte layout the other doesn't produce - since nothing
else in the suite compares the two scenarios' on-disk output directly.
