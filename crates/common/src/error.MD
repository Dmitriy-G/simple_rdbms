# error

The single error type threaded through every layer of the engine. Each
downstream crate defines its own, more specific error enum for the
failures it can produce, and converts that local error into a variant here
(via `impl From<LocalError> for Error`, matching by hand rather than
`.to_string()`-ing the whole thing) at the point where it crosses into a
caller from another crate. Keeping one top-level error type means the
`engine` facade and the `cli` REPL only ever need to match on one shape -
and, since every variant is a structured, SQL-condition-named value rather
than a formatted sentence, that one shape is also machine-readable: a
future JDBC/ODBC server can hand a client a SQLSTATE without parsing
prose. See `sql_state.MD` for the code type itself and
`docs/adr/0002-crate-splitting.md` for why the crate this lives in must
stay free of dependencies a higher layer would have to reach back down
through.

Every variant is named after the SQL condition it represents
(`UndefinedTable`, not `Catalog`; `DatatypeMismatch`, not `Binder`), so two
different crates that hit the same condition converge on the same
variant - `planner::PlannerError::UnknownTable` and
`catalog::CatalogError::TableNotFound` both become `Error::UndefinedTable`,
because from a SQL client's perspective they are the same failure. This is
also why the four crate-level errors that predate this design
(`StorageError`, `SqlError`, `PlannerError`, `CatalogError`, `TxnError`,
`ExecutorError`) were not restructured themselves: they stay as detailed,
crate-local diagnostics, and only their `From` conversion into `Error`
had to change, from stringifying to matching.

## Key Components

- `Error` - the enum. Each variant below is listed as
  `Variant -> SQLSTATE, severity`; see the variant's own doc comment in
  source for its fields. Fatal variants represent state the engine can no
  longer trust (corruption, a lost write, a broken invariant); Error
  variants are scoped to the one statement that raised them.
  - `Io` -> `IO_ERROR` (58030), Fatal. A filesystem or OS-level failure
    that wasn't already caught and re-classified by `storage`.
  - `ChecksumMismatch`, `CorruptPage`, `CorruptLog`, `TruncatedFile`,
    `PageNotFound` -> `DATA_CORRUPTED` (XX001), Fatal. On-disk bytes that
    don't match what the format guarantees, from
    `storage::StorageError`'s corresponding variants.
  - `DoubleWriteRestoreFailed` -> `IO_ERROR` (58030), Fatal. A torn page's
    own repair copy didn't survive being written either.
  - `BufferPoolExhausted` -> `OUT_OF_MEMORY` (53200), Error. Every frame
    is pinned; the statement fails, the session doesn't.
  - `TupleTooLarge` -> `PROGRAM_LIMIT_EXCEEDED` (54000), Error.
  - `KeyTooLarge` -> `PROGRAM_LIMIT_EXCEEDED` (54000), Error. An index key's
    encoded bytes are larger than a single B+tree node
    (`storage::btree::BTreeIndex::insert`) could ever hold, checked before
    any page I/O happens rather than discovered partway through a split.
  - `InvalidConfiguration` -> `CONNECTION_FAILURE` (08006), Fatal. A
    `DbConfig` value the storage layer rejected while opening (e.g.
    `storage::StorageError::InvalidDwbCapacity`) - from a future client's
    perspective, this is why the session couldn't be established.
  - `Syntax` -> `SYNTAX_ERROR` (42601), Error. From `sql::SqlError`,
    rendered against the original source text; `offset` is the byte
    position the lexer/parser was at, preserved as data rather than only
    baked into `message`.
  - `ColumnCountMismatch` -> `SYNTAX_ERROR` (42601), Error. An `INSERT`
    whose value-tuple length doesn't match its column list.
  - `UndefinedTable`, `UndefinedColumn`, `AmbiguousColumn`,
    `DuplicateTable`, `DatatypeMismatch` -> `UNDEFINED_TABLE` (42P01),
    `UNDEFINED_COLUMN` (42703), `AMBIGUOUS_COLUMN` (42702),
    `DUPLICATE_TABLE` (42P07), `DATATYPE_MISMATCH` (42804) respectively,
    all Error. From `planner::PlannerError`/`catalog::CatalogError`'s name
    and type-resolution failures, and `executor::ExecutorError::Evaluation`
    for a type error caught at execution rather than bind time.
  - `UndefinedIndex`, `DuplicateIndex` -> `UNDEFINED_OBJECT` (42704),
    `DUPLICATE_OBJECT` (42710), both Error. From
    `catalog::CatalogError::IndexNotFound`/`IndexAlreadyExists`
    (`catalog::catalog.MD`). Unlike `UndefinedTable`/`DuplicateTable`, the
    standard has no class-42 code specific to an index, so these use the
    standard's own generic "object" codes rather than a Postgres-style
    `42P`-prefixed extension code (see `sql_state.MD`).
  - `NumericValueOutOfRange` -> `NUMERIC_VALUE_OUT_OF_RANGE` (22003),
    Error. A literal that doesn't fit its column's type -
    `planner::PlannerError::LiteralOutOfRange` - kept distinct from
    `DatatypeMismatch` so a client can tell "wrong shape" from "wrong
    magnitude" apart.
  - `DataCorrupted` -> `DATA_CORRUPTED` (XX001), Fatal. Catalog-level
    corruption (`catalog::CatalogError::Corrupt`) or a decoded tuple that
    doesn't match its schema (`executor::ExecutorError::CorruptTuple`) -
    distinct variants from the `storage`-level corruption cases above
    because the fields differ, but the same SQLSTATE, since both mean the
    same thing to a client.
  - `SerializationFailure` -> `SERIALIZATION_FAILURE` (40001), Error,
    **retryable**. A deadlock victim (`txn::TxnError::DeadlockVictim`);
    named after the SQLSTATE class rather than the mechanism because a
    retrying client only needs to know "conflict, try again."
  - `Internal` -> `INTERNAL_ERROR` (XX000), Fatal. An invariant this
    engine's own bookkeeping should never violate
    (`txn::TxnError::UnknownTransaction`/`LockAfterUnlock`) - reachable
    only by a bug here, not by anything a client did.
  - `NestedTransaction`, `NoActiveTransaction` -> `NO_ACTIVE_SQL_TRANSACTION`
    (25P01), Error. `BEGIN` while already inside a transaction, or
    `COMMIT`/`ROLLBACK` with none open.
  - `TransactionAborted` -> `IN_FAILED_SQL_TRANSACTION` (25P02), Error.
    Returned by every statement issued after one has already failed
    inside an explicit transaction, until `ROLLBACK`. See
    `engine::database.MD` for the state machine that produces this.
  - `NotSupported` -> `FEATURE_NOT_SUPPORTED` (0A000), Error. A
    well-formed request for functionality that doesn't exist yet - kept
    distinct so callers (and tests) can assert "not implemented" without
    it being confused for a real failure.
- `Error::sql_state(&self) -> SqlState` - the exhaustive match described
  above: every variant maps to exactly one `SqlState` constant, and a new
  variant fails to compile here until it's given one.
- `Error::severity(&self) -> Severity` - `Error` or `Fatal` per variant,
  per the table above; matches the wire-protocol severities so a future
  server layer knows whether the *connection* survives a given failure.
- `Error::is_retryable(&self) -> bool` - `true` only when `sql_state()` is
  `SqlState::SERIALIZATION_FAILURE` or `SqlState::STATEMENT_COMPLETION_UNKNOWN` -
  the two class-40 codes the standard defines specifically so a client
  knows to retry the whole transaction rather than surface the failure.
- `Error::redacted(&self) -> String` - the same message `Display` would
  produce, except every field that can hold a user-supplied value is
  replaced with `?`; schema identifiers (table names, column names, data
  types) are left in place, since those are structure an operator needs,
  not data a user typed. Three variants change: `Syntax` drops `message`
  entirely (the offending token can be anywhere in that free-text string)
  and keeps only `offset`; `DatatypeMismatch` drops `detail` entirely for
  the same reason - `executor::ExecutorError::Evaluation` builds it from a
  `{value:?}` Debug format, so a literal can be quoted anywhere inside it;
  `NumericValueOutOfRange` keeps `column` and `data_type` but replaces
  `value`. Every other variant returns `self.to_string()` unchanged. The
  match has no wildcard arm - every variant is named individually, in the
  same style as `sql_state`/`severity` above - so a new variant fails to
  compile here until someone decides which bucket it belongs in; see
  `tests/redaction.MD` for the test that pins this down with a sentinel
  value. This exists because `engine::Database::execute` logs at WARN on
  every statement failure, and WARN is a level operators run permanently,
  unlike the `debug`-only full `%err` - see CLAUDE.md's logging section
  and `engine/src/database.MD`.
- `Severity` - `Error` (the statement failed, the connection survives) |
  `Fatal` (the connection must close) | `Panic` (unused today; reserved
  for a failure that would take down every connection, which nothing in
  a single-process engine can cause).
- `Result<T>` - a convenience alias for `std::result::Result<T, Error>`,
  used throughout the workspace instead of spelling that out.

## Usage Example

```rust
use common::{Error, Result, Severity};

fn run() -> Result<()> {
    do_storage_thing().map_err(|e| Error::TupleTooLarge { size: e.0, max: e.1 })?;
    Ok(())
}
# fn do_storage_thing() -> std::result::Result<(), (usize, usize)> { Ok(()) }

fn report(err: &Error) {
    eprintln!("[{}] {err}", err.sql_state());
    if err.severity() == Severity::Fatal {
        eprintln!("connection must close");
    }
}
```

Every crate above `common` in the dependency graph converts its own local
error type into this one at its boundary, matching each local variant to
the `Error` variant naming the same SQL condition, so
`engine::Database::execute` and the `cli` REPL's error-printing path only
ever have one enum - carrying one SQLSTATE and one severity - to match on,
regardless of which layer actually failed. `engine::Database` is also the
only place an `Error` is logged - see CLAUDE.md's "log at the boundary"
rule: every intermediate `From` conversion here is silent by design, so a
single failure is reported once, not once per layer it passed through.
