# sql_state

Proves `Error::sql_state`/`Error::is_retryable` stay correct as `common::Error`
grows, the same way `scripts/check_docs.sh`'s coverage check proves the
`.MD` files do: by making the missing case a compile error, not a runtime
gap nobody notices.

## Key Components

- `sample_errors() -> Vec<Error>` - one concrete instance of every
  `Error` variant, with arbitrary but valid field values.
- `expected_sql_state(err) -> SqlState` - a second, independent exhaustive
  match from every `Error` variant to the `SqlState` it should carry, kept
  deliberately separate from `Error::sql_state`'s own match rather than
  calling it: if a new `Error` variant is added, *this* match fails to
  compile until it's given an arm too, so the test can't pass by
  accident just because production code was updated and the test wasn't.
- `every_error_variant_maps_to_its_sql_state` - asserts `err.sql_state()`
  equals `expected_sql_state(&err)` for every sample.
- `is_retryable_is_true_only_for_class_40_codes` - asserts
  `err.is_retryable()` equals "this sample's SQLSTATE is
  `SERIALIZATION_FAILURE` or `STATEMENT_COMPLETION_UNKNOWN`" for every
  sample - since only one sample (`SerializationFailure`) is currently a
  class-40 code, this also pins down that `is_retryable` is `false` for
  the other twenty-six.

## Usage Example

```rust
use common::{Error, SqlState};

let err = Error::UndefinedTable { name: "missing".to_string() };
assert_eq!(err.sql_state(), SqlState::UNDEFINED_TABLE);
assert!(!err.is_retryable());
```

Run just this test file with `cargo test -p common --test sql_state`; it
is also part of `cargo test --workspace` like every other integration
test.
