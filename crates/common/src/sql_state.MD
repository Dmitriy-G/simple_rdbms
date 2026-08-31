# sql_state

The SQLSTATE codes this database can actually emit, as a typed constant
per code rather than an ad hoc string threaded through error messages.
SQLSTATE is the five-character, alphanumeric error-identity code shared by
the SQL standard, PostgreSQL's wire protocol, JDBC, and ODBC — giving
`common::Error` this type now, before any server or client protocol work
exists, means that work never has to retrofit machine-readable error
identity across the eight crates that can raise one. `common::Error::sql_state`
(see `error.MD`) is the only place these constants are consumed; nothing
else in the workspace should format its own five-character code by hand.

This module only defines codes this engine can currently raise, or is
about to (see `error.MD` for exactly which `Error` variant uses which),
grouped by class exactly as the standard does — it is deliberately not a
transcription of the full SQLSTATE table. A few codes here have no
`Error` variant pointing at them yet (`NULL_VALUE_NOT_ALLOWED`,
`NOT_NULL_VIOLATION`, `UNIQUE_VIOLATION`, `DISK_FULL`, `QUERY_CANCELED`,
`ADMIN_SHUTDOWN`) because the behavior that would raise them — constraint
enforcement beyond nullability bookkeeping, disk-space accounting, statement
cancellation, admin shutdown — doesn't exist yet either; they're defined
ahead of time so that future work maps onto a code that's already reserved
rather than inventing one under pressure. `PROGRAM_LIMIT_EXCEEDED`
(class 54) and `AMBIGUOUS_COLUMN`/`FEATURE_NOT_SUPPORTED`/
`NO_ACTIVE_SQL_TRANSACTION`/`INTERNAL_ERROR` are not in the task's own
worked example list but were added the same way that list added `54000`:
each is the standard's own code for an `Error` variant this crate already
has to map (`TupleTooLarge`, `AmbiguousColumn`, `NotSupported`,
`NestedTransaction`/`NoActiveTransaction`, and the two unreachable-but-real
`TxnError` arms, respectively). `UNDEFINED_OBJECT`/`DUPLICATE_OBJECT` were
added the same way for `Error::UndefinedIndex`/`DuplicateIndex`: the
standard has no class-42 code specific to an index (unlike `_TABLE`), so
`42704`/`42710` - the generic "undefined object"/"duplicate object" codes
Postgres itself uses for `DROP INDEX`/`CREATE INDEX` errors - are the
correct, standard-sanctioned choice rather than inventing a Postgres-style
`42P`-prefixed extension code for something the standard already covers.
`IDLE_IN_TRANSACTION_SESSION_TIMEOUT` (`25P03`) is Postgres's own extension
code for exactly this condition - an explicit transaction the engine aborts
on its own because no statement arrived before `DbConfig::idle_in_transaction_timeout_ms`
elapsed - and maps to `Error::IdleInTransactionTimeout`.

## Key Components

- `SqlState` - a newtype over `[u8; 5]`, the raw ASCII bytes of one
  SQLSTATE code. `SqlState::as_str(&self) -> &str` views those bytes as
  UTF-8 (every constant here is ASCII, so this never actually fails; it
  falls back to `"?????"` rather than panicking if it somehow did).
  `SqlState::class(&self) -> &str` is the first two characters, the
  code's class (e.g. `"42"` for every syntax/access-rule-violation code
  below) - the granularity a client typically switches on before caring
  about the specific five-character code.
- Associated constants, one per code, named after the standard's own
  condition name in `SCREAMING_SNAKE_CASE` (e.g. `UNDEFINED_TABLE` for
  `42P01`) rather than the class number, so a reader matches on what the
  condition means without cross-referencing a table. Every constant,
  code, and class:

  | Constant | Code | Class |
  | --- | --- | --- |
  | `SUCCESSFUL_COMPLETION` | `00000` | 00 - successful completion |
  | `CONNECTION_FAILURE` | `08006` | 08 - connection exception |
  | `NUMERIC_VALUE_OUT_OF_RANGE` | `22003` | 22 - data exception |
  | `NULL_VALUE_NOT_ALLOWED` | `22004` | 22 - data exception |
  | `INVALID_TEXT_REPRESENTATION` | `22P02` | 22 - data exception |
  | `NOT_NULL_VIOLATION` | `23502` | 23 - integrity constraint violation |
  | `UNIQUE_VIOLATION` | `23505` | 23 - integrity constraint violation |
  | `NO_ACTIVE_SQL_TRANSACTION` | `25P01` | 25 - invalid transaction state |
  | `IN_FAILED_SQL_TRANSACTION` | `25P02` | 25 - invalid transaction state |
  | `IDLE_IN_TRANSACTION_SESSION_TIMEOUT` | `25P03` | 25 - invalid transaction state |
  | `LOCK_NOT_AVAILABLE` | `55P03` | 55 - object not in prerequisite state |
  | `SERIALIZATION_FAILURE` | `40001` | 40 - transaction rollback |
  | `STATEMENT_COMPLETION_UNKNOWN` | `40003` | 40 - transaction rollback |
  | `SYNTAX_ERROR` | `42601` | 42 - syntax error or access rule violation |
  | `AMBIGUOUS_COLUMN` | `42702` | 42 - syntax error or access rule violation |
  | `UNDEFINED_COLUMN` | `42703` | 42 - syntax error or access rule violation |
  | `UNDEFINED_TABLE` | `42P01` | 42 - syntax error or access rule violation |
  | `DUPLICATE_TABLE` | `42P07` | 42 - syntax error or access rule violation |
  | `DATATYPE_MISMATCH` | `42804` | 42 - syntax error or access rule violation |
  | `UNDEFINED_OBJECT` | `42704` | 42 - syntax error or access rule violation |
  | `DUPLICATE_OBJECT` | `42710` | 42 - syntax error or access rule violation |
  | `FEATURE_NOT_SUPPORTED` | `0A000` | 0A - feature not supported |
  | `DISK_FULL` | `53100` | 53 - insufficient resources |
  | `OUT_OF_MEMORY` | `53200` | 53 - insufficient resources |
  | `PROGRAM_LIMIT_EXCEEDED` | `54000` | 54 - program limit exceeded |
  | `QUERY_CANCELED` | `57014` | 57 - operator intervention |
  | `ADMIN_SHUTDOWN` | `57P01` | 57 - operator intervention |
  | `IO_ERROR` | `58030` | 58 - system error |
  | `INTERNAL_ERROR` | `XX000` | XX - internal error |
  | `DATA_CORRUPTED` | `XX001` | XX - internal error |
  | `INDEX_CORRUPTED` | `XX002` | XX - internal error |

## Usage Example

```rust
use common::SqlState;

let state = SqlState::UNDEFINED_TABLE;
assert_eq!(state.as_str(), "42P01");
assert_eq!(state.class(), "42");
```

`Error::sql_state(&self) -> SqlState` (see `error.MD`) is an exhaustive
match from every `Error` variant to one of these constants; a new `Error`
variant fails to compile until it's given one. Nothing downstream is
expected to construct a `SqlState` directly - only to read `.as_str()` off
one once a client-facing protocol exists to send it over.
