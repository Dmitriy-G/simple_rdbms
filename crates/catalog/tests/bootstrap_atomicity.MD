# bootstrap_atomicity

The catalog's own bootstrap heap allocation (`AllocPage`) and the database
header's `catalog_first_page` pointer to it (a page-0 `Update`) are two
separate WAL records that must become durable together or not at all —
per the storage engine's original design task's "route page 0 through the
WAL" guidance: a header pointing at a heap whose allocation never survived
a crash would be a dangling pointer, not just wasted space. Sweeps every
possible crash point across the very first ever open of a fresh database,
since that's the one moment this sequence runs.

## Key Components

- `BOOTSTRAP_TXN` - the fixed `TxnId(0)` the bootstrap sequence runs
  under, matching what `Database::open_with_managers` uses in practice.
- `open_file(path) -> std::io::Result<std::fs::File>` - private. Opens (or
  creates) a file for read/write without truncating it.
- `faulty_pool(dir, counter, fail_at) -> Result<BufferPool, Box<dyn Error>>` -
  private. Builds a `BufferPool` whose database and WAL files are wrapped
  in `FaultyDevice`, sharing one write counter so the pair fails together
  once `fail_at` writes have happened across either device.
- `real_pool(dir) -> Result<BufferPool, Box<dyn Error>>` - private. Builds
  an ordinary, non-faulty `BufferPool` against the same directory, used to
  reopen and recover after a simulated crash.
- `run_bootstrap(pool) -> Result<(), Box<dyn Error>>` - private. Exactly
  what `Database::open_with_managers` does the very first time a database
  is ever opened: begin a real transaction, let `Catalog::open` provision
  its bootstrap heap (and point the header at it) if needed, then commit.
- `total_bootstrap_write_count(dir) -> Result<u64, Box<dyn Error>>` -
  private. Runs `run_bootstrap` to completion against a fault-free,
  counting device pair, returning the number of writes it performs — the
  upper bound for the crash-injection sweep below.
- `the_bootstrap_heap_and_its_header_pointer_become_durable_together_or_not_at_all` -
  for every crash point `n` from 1 up to the total write count, runs
  `run_bootstrap` against a `faulty_pool` that fails at write `n` (an
  `Err` here is expected and not itself a test failure — even constructing
  the pool does its own durable writes), then reopens with a `real_pool`,
  runs recovery, and checks the header's `catalog_first_page`: if it's
  `None`, that's fine — a fresh one is provisioned next time; if it's
  `Some`, `Catalog::open` must succeed against it and find zero tables,
  proving the pointer never dangles.

## Usage Example

```rust
#[test]
fn example_sweeps_one_crash_point() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let counter = std::rc::Rc::new(std::cell::Cell::new(0));
    let pool = faulty_pool(dir.path(), &counter, 3)?;
    let _ = run_bootstrap(&pool);
    let recovered = real_pool(dir.path())?;
    storage::recovery::recover(&recovered)?;
    Ok(())
}
```

Run as part of `cargo test --workspace`; the single test in this file is
the crate's crash-safety proof for the one sequence every database's
lifetime depends on — its very first open — complementing
`tests/persistence.rs`, which checks ordinary (non-crashing) round trips.
