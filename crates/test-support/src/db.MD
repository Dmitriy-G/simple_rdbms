# db

The one line every `engine`-level test's own `config` helper repeated
before diverging into whatever it actually wanted to override.

## Key Components

- `db_config(dir) -> common::DbConfig` - `DbConfig::new(dir.join("test.db"))`.
  Callers that need something other than the defaults apply
  `common::DbConfig`'s own struct-update syntax on top of this rather than
  needing a second parameterised builder here -
  `common::DbConfig { checkpoint_byte_threshold: 1, ..test_support::db_config(dir) }`
  is exactly as short as a bespoke `config(dir)` function that only ever
  set one field, and it stays that way as more fields need overriding at a
  given call site, which a fixed-signature helper function would not.

## Usage Example

```rust
use test_support::db_config;

let dir = tempfile::tempdir()?;
let config = db_config(dir.path());
let mut db = engine::Database::open(config)?;
```

`crates/engine/tests/flush_poisoning.rs` and
`crates/engine/tests/rollback_matches_recovery_undo.rs` both build their
`common::DbConfig` this way; `crates/engine/tests/crash_injection.rs`
layers its own `HarnessConfig` (WAL segment size, checkpoint threshold,
buffer pool size) on top via the same struct-update pattern.
