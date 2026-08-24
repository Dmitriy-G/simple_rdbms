# smoke

Asserts the crate compiles and its main type can be constructed.

## Key Components

- `database_opens_and_closes` - opens a `Database` against a temp-dir
  file and closes it, checking neither panics.

## Usage Example

```rust
#[test]
fn example_opens_a_database() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let config = common::DbConfig::new(dir.path().join("smoke.db"));
    let db = engine::Database::open(config).expect("open database");
    db.close().expect("close database");
}
```

Run as part of `cargo test --workspace`; catches a broken build or a
signature change in `Database::open`/`Database::close` before it surfaces
indirectly in a downstream crate's tests.
