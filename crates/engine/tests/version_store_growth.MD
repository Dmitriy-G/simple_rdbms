# version_store_growth

Drives `engine::Database` through real SQL statements and asserts the
end-to-end property `docs/adr/0013-version-identity-and-lifetime.md`
("Lifetime") and M10.4 exist to deliver: `txn::VersionStore`'s map is
bounded by *live, contended* rows rather than by every row ever inserted.
`crates/txn/tests/lifecycle.rs` already proves the pruning mechanism
itself against a bare `TransactionManager`; this file proves the same
thing is actually wired all the way through `engine::Database::execute`,
autocommit included.

## Key Components

- `AUTOCOMMIT_ROW_COUNT` - private. `300`, chosen to be "several hundred"
  per the sub-milestone's own wording - large enough that a regression
  back to whole-map scanning or unbounded growth would be obvious, not
  just theoretically present.
- `version_chains_stay_bounded_across_autocommit_inserts_and_an_open_transaction` -
  inserts `AUTOCOMMIT_ROW_COUNT` rows, each its own autocommit statement
  (and therefore its own transaction, committing and leaving the active
  set immediately), and asserts `Database::stats()?.version_chains` is
  exactly `0` once the last one lands - not merely smaller than the row
  count, which a partial fix could still satisfy. It then opens a second
  session, begins an explicit transaction, inserts twenty more rows, and
  asserts `version_chains` is greater than zero while that transaction is
  still open - its own uncommitted versions must have live chains -
  before committing it and asserting the count drops back to `0`.

## Usage Example

```rust
use common::DbConfig;
use engine::Database;

let mut db = Database::open(DbConfig::new(path))?;
db.execute("CREATE TABLE t (a INTEGER)")?;
for i in 0..300 {
    db.execute(&format!("INSERT INTO t VALUES ({i})"))?;
}
assert_eq!(db.stats()?.version_chains, 0);
```

Run as part of `cargo test --workspace`, or in isolation with
`cargo test -p engine --test version_store_growth`. Needs
`Database::stats`, gated `#[cfg(any(test, feature = "test-util"))]`:
`crates/engine/Cargo.toml`'s self-referencing `engine` dev-dependency with
`test-util` enabled (which also forwards `txn/test-util`) is what makes
`EngineStats::version_chains` visible here, the same way it already makes
`checkpoints_written` visible to `tests/sessions.rs`.
