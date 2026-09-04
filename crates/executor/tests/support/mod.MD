# mod

Shared test-only helpers for `executor`'s own `tests/` integration suite,
declared with `mod support;` by whichever test file needs them. Nothing
here is gated behind `#[cfg(test)]` in `src/` — that attribute only takes
effect when compiling the crate's own unit-test binary, so it is invisible
to `tests/`, which links against an ordinary (non-`--cfg test`) build of
the crate; see `storage::tests::support::mod.MD` for the same rationale
and CLAUDE.md's testing section for when `#[cfg(test)]` in `src/` is still
the right call. Each function here is still individually marked
`#[cfg(test)]`, matching `crates/executor/tests/seq_scan.rs`'s existing
helpers - not for cfg-gating (the whole file is already test-only) but
because `clippy.toml`'s `allow-expect-in-tests`/`allow-unwrap-in-tests`
only suppresses `clippy::expect_used`/`unwrap_used` inside code it
recognizes as test code, and a plain private helper function with no
`#[test]`/`#[cfg(test)]` attribute of its own does not qualify even inside
an integration-test binary.

## Key Components

- `open_pool(pool_size) -> (BufferPool, tempfile::TempDir)` - the same
  tempfile-backed `DiskManager`/`DoubleWriteBuffer`/`LogManager`/
  `BufferPool` construction `tests/seq_scan.rs` already had inline before
  this module existed; other test files construct the same thing, so it
  moved here rather than staying duplicated.
- `run_to_completion(pool, catalog, txn, executor) -> Vec<Tuple>` - builds
  one `ExecutorContext` (against a fresh, ephemeral `txn::LockManager`
  created just for this call - these tests exercise index/scan mechanics,
  not locking, so nothing needs a lock manager shared across separate
  `run_to_completion` calls the way `engine::EngineShared` shares one
  real `LockManager` across a whole database), calls `executor.init`, then
  drains `executor.next` until it returns `None`, collecting every yielded
  `Tuple`. Panics (via `.expect`) on any `Err` from `init`/`next` - only
  suitable for tests asserting a executor runs to completion successfully;
  a test that needs to observe an `Err` (e.g. an index-insert failure)
  drives the executor's `init`/`next` calls by hand instead.
- `row(values) -> Vec<BoundExpr>` - wraps each `Value` in `BoundExpr::Literal`,
  building an `InsertExecutor`-ready row directly (bypassing `sql`/`planner`
  binding entirely, since these tests only need a specific `Value` in a
  specific column, not full statement parsing).
- `encoded(value) -> Vec<u8>` - `value.encode_memcomparable`, panicking
  (via `.expect`) on failure; used to build the same index key an
  `InsertExecutor`/`IndexScanExecutor` would derive, for asserting against
  `storage::btree::BTreeIndex::get`/constructing scan bounds directly. Not
  suitable for a test that expects encoding itself to fail (a NaN
  `Double`) - such a test calls `encode_memcomparable` directly instead.

## Usage Example

```rust
mod support;

use catalog::Catalog;
use common::TxnId;
use executor::{InsertExecutor, SeqScanExecutor};
use txn::{IsolationLevel, Transaction};

let (pool, _dir) = support::open_pool(16);
let mut catalog = Catalog::new();
// ... catalog.create_table / create_index ...
let txn = Transaction::new(TxnId(0), IsolationLevel::ReadCommitted);
let mut insert = InsertExecutor::new(table_id, rows);
support::run_to_completion(&pool, &catalog, &txn, &mut insert);
```

`crates/executor/tests/index_maintenance.rs` and
`crates/executor/tests/index_scan.rs` both declare `mod support;` and use
these helpers to seed a table/index and drive `InsertExecutor`/
`IndexScanExecutor` without repeating the setup boilerplate
`tests/seq_scan.rs` established.
