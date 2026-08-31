# mod

Shared test-only helpers for `engine`'s own `tests/` integration suite,
declared with `mod support;` by whichever test file needs them. Nothing
here is gated behind `#[cfg(test)]` in `src/` - that attribute only takes
effect when compiling the crate's own unit-test binary, so it is invisible
to `tests/`, which links against an ordinary (non-`--cfg test`) build of
the crate; see `crates/executor/tests/support/mod.MD` for the same pattern
used by another crate's suite - `mod support;` only resolves within one
crate's `tests/` directory, so it cannot be shared across crates, which is
why each crate that needs it carries its own copy.

This module previously held `CaptureBuf`/`captured_events`/
`set_capturing_subscriber` too; those moved to the shared `test-support`
crate (`crates/test-support/src/logging.MD`) once `catalog`, `executor`,
and `txn`'s own `tests/` needed the identical capture rig and a
crate-local `mod support;` couldn't reach across crate boundaries. What's
left here is `engine`-specific: converting one test's `ResultSet` into the
plain `Vec<Value>` shape most assertions actually compare against, which
needs `engine::ResultSet` and so can never move into `test-support`
(`crates/test-support/README.md`'s own architecture section - it depends
on `common` and `storage` only, never `engine`, precisely to keep the
dev-dependency edge one-directional).

Only `rows_of` lives here, not the column-returning variant
`integration.rs` also needs (`(Vec<String>, Vec<Vec<Value>>)`). That
variant stays private to `integration.rs` deliberately: it has exactly one
caller, so it isn't duplicated anywhere and doesn't belong in a *shared*
module - and rustc's dead-code check is per test binary (each `tests/*.rs`
file is its own crate), so a shared `pub fn` only some `mod support;`
declarers call is flagged unused in every binary that doesn't call it.
Moving it here anyway would need either an `#[allow(dead_code)]`
CLAUDE.md's lint-suppressions section already closes the door on, or
forcing every consumer of this module to call every function in it
whether or not the test needs it - `rows_of`'s three consumers already
satisfy that by accident (all three call it), which is what makes sharing
it safe and the tuple-returning variant not.

## Key Components

- `rows_of(result) -> Vec<Vec<Value>>` - unwraps a `ResultSet::Rows`,
  discarding its column names, into one `Vec<Value>` per row; panics
  (naming the actual variant) if `result` is `RowsAffected` or
  `RolledBack`, since every caller already expects a `SELECT`-shaped
  result and a mismatch there is a test bug worth failing loudly on
  rather than silently returning nothing.

## Usage Example

```rust
mod support;
use support::rows_of;

let mut db = open(&dir);
db.execute("CREATE TABLE t (a INTEGER)").expect("create table");
let result = db.execute("SELECT * FROM t").expect("select");
assert_eq!(rows_of(result), Vec::<Vec<types::Value>>::new());
```

`crates/engine/tests/transactions.rs`, `explain.rs`, and `index_scan.rs`
all declare `mod support;` and use `rows_of` this exact way - it used to
be a byte-identical private function copied into each of those three
files (plus a fourth, incompatible copy in `integration.rs` that also
returned column names, which stays local to that file - see above).
