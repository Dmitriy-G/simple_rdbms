# redaction

Proves `Error::redacted` actually redacts, the same way `sql_state.rs`
proves `Error::sql_state` stays correct: by asserting on concrete values
rather than trusting the implementation's own intent. A log line built
from `redacted()` is the one that reaches WARN in
`engine::Database::execute` (see `engine/src/database.MD`), so a
regression here is a data-exfiltration regression, not just a cosmetic
one.

## Key Components

- `SENTINEL` - a string unlikely to appear by coincidence, planted as the
  user-supplied value in each sample so its absence from `redacted()`'s
  output is a meaningful assertion rather than a vacuous one.
- `value_bearing_errors() -> Vec<Error>` - one instance of every `Error`
  variant CLAUDE.md's error-handling section calls out as carrying a
  user-supplied value: `Syntax` (the offending token lives in `message`),
  `DatatypeMismatch` (a literal may be quoted inside `detail`), and
  `NumericValueOutOfRange` (the out-of-range `value` itself).
- `redacted_never_contains_the_sentinel_value` - asserts `SENTINEL` is
  absent from `redacted()`'s output for every sample in
  `value_bearing_errors`.
- `redacted_keeps_schema_identifiers` - asserts `NumericValueOutOfRange`'s
  redacted form still names the column and data type, only the literal
  value is replaced with `?` - schema identifiers are structure, not
  data, per CLAUDE.md.
- `redacted_keeps_non_value_bearing_variants_unchanged` - asserts a
  variant with no user-supplied value (`UndefinedTable`) redacts to
  exactly its normal `Display` output, so `redacted()` isn't over-eager.

## Usage Example

```rust
use common::Error;

let err = Error::NumericValueOutOfRange {
    column: "age".to_string(),
    value: "99999999999".to_string(),
    data_type: "INTEGER".to_string(),
};
assert_eq!(err.redacted(), "value ? out of range for column age (INTEGER)");
```

Run just this test file with `cargo test -p common --test redaction`; it
is also part of `cargo test --workspace` like every other integration
test.
