# error

The single error type threaded through every layer of the engine. Each
downstream crate defines its own, more specific error enum for the
failures it can produce, and converts that local error into a variant here
(typically via `impl From<LocalError> for Error`) at the point where it
crosses into a caller from another crate. Keeping one top-level error type
means the `engine` facade and the `cli` REPL only ever need to match on one
shape.

## Key Components

- `Error` - the enum. `Io` wraps a filesystem or OS-level I/O failure
  (opening/reading/writing the database file, WAL segments, etc.) and
  converts automatically via `#[from] std::io::Error`. `Storage` carries a
  failure inside the storage engine: disk manager, buffer pool, page
  layout, heap file, B+tree, or WAL. `Parse` carries a failure while
  lexing or parsing SQL text into an AST. `Binder` carries a failure while
  binding a parsed AST against the catalog (unknown table/column, type
  mismatch, ambiguous reference, etc.). `Execution` carries a failure
  raised by the query executor while pulling tuples through an operator
  tree. `Transaction` carries a failure in the transaction subsystem: lock
  manager, MVCC, or transaction lifecycle management. `Catalog` carries a
  failure in the system catalog: table/column already exists, unknown
  table, schema mismatch, etc. `NotSupported` marks a request that is
  well-formed but exercises functionality that does not exist yet — kept
  distinct from the other variants so callers (and tests) can assert "not
  implemented" without it being confused for a real failure.
- `Result<T>` - a convenience alias for `std::result::Result<T, Error>`,
  used throughout the workspace instead of spelling that out.

## Usage Example

```rust
use common::{Error, Result};

fn run() -> Result<()> {
    do_storage_thing().map_err(|e| Error::Storage(e.to_string()))?;
    Ok(())
}
# fn do_storage_thing() -> std::result::Result<(), String> { Ok(()) }
```

Every crate above `common` in the dependency graph converts its own local
error type into this one at its boundary, so `engine::Database::execute`
and the `cli` REPL's error-printing path only ever have one enum to match
on, regardless of which layer actually failed.
