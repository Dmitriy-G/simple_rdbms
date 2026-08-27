# lib

Cross-cutting types shared by every crate in the workspace: the unified
error type and its `Result` alias, newtype identifiers for the core
entities that flow between subsystems, and top-level database
configuration. This is the foundation every other crate builds on — see
`docs/adr/0002-crate-splitting.md` for the full layered dependency graph —
and it sits at the very bottom of that graph: `common` must never depend on
any other workspace crate, so nothing it exports can carry a dependency
edge back up into a higher layer.

## Key Components

- `DbConfig` - top-level configuration for opening a database. See
  `config.MD`.
- `Error` / `Result<T>` - the unified error type threaded through every
  layer, and its result alias. See `error.MD`.
- `Severity` - `Error`/`Fatal`/`Panic`, an `Error`'s wire-protocol
  severity. See `error.MD`.
- `SqlState` - the SQLSTATE code identifying an `Error`'s condition. See
  `sql_state.MD`.
- `PageId`, `FrameId`, `TxnId`, `TableId`, `IndexId`, `ColumnId`, `Lsn`,
  `Rid` - newtype identifiers for the core entities that flow between
  subsystems. See `ids.MD`.
- `crc` - a hand-written CRC-32 implementation, exposed as its own
  submodule rather than re-exported flat, since it is a utility rather
  than a core domain type. See `crc.MD`.
- `sync` - a poisoned-lock recovery helper shared by every layer that
  synchronizes with `std::sync` primitives, exposed as its own submodule
  for the same reason as `crc`. See `sync.MD`.

## Usage Example

```rust
use common::{DbConfig, PageId, Result};

fn open(path: &str) -> Result<()> {
    let config = DbConfig::new(path);
    let _first_page = PageId(0);
    // ... pass `config` down into `storage::disk::DiskManager::open`, etc.
    Ok(())
}
```

Every other crate in the workspace depends on `common` and imports these
types directly rather than redefining its own; `engine` (and, through it,
`cli`) is the one that actually constructs a `DbConfig` and threads it down
into the disk manager and buffer pool at startup.
