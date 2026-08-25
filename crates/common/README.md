# common

Shared foundation for every crate in the workspace: the unified error type,
newtype identifiers for the core entities that flow between subsystems, and
top-level database configuration.

## Architecture

`common` sits at the bottom of the layered workspace (see
`docs/adr/0002-crate-splitting.md`): it depends on no other workspace crate,
and every other crate depends on it, directly or transitively. That makes it
the one place a type shared across layers — an error, an id, a config knob
— can live without creating a back-edge in the dependency graph. Nothing it
exports may carry a dependency on a higher layer, which is why it stays
deliberately small: a change here is felt by the whole workspace.

## Key Components

- `error` - `Error`, the unified error type every layer's own error
  converts into, and its `Result<T>` alias. See
  [error.MD](src/error.MD).
- `ids` - `PageId`, `FrameId`, `TxnId`, `TableId`, `ColumnId`, `Lsn`, `Rid`,
  the newtype identifiers passed between subsystems. See
  [ids.MD](src/ids.MD).
- `config` - `DbConfig`, top-level configuration for opening a database. See
  [config.MD](src/config.MD).
- `crc` - a hand-written CRC-32 implementation, kept as its own submodule
  rather than re-exported flat since it's a utility, not a domain type. See
  [crc.MD](src/crc.MD).

## Features

`common` has no `todo!()`s and nothing stubbed — it is plain data types and
one small algorithm (CRC-32), not behavior that grows with the roadmap.

## Dependencies

No workspace crates — `common` is the bottom of the dependency graph.
External: `thiserror`, for deriving `Error`'s `Display`/`std::error::Error`
impls without hand-writing them.

## Configuration

`common` itself has no configuration; it defines the `DbConfig` struct that
every other layer's configuration is expressed through. See
[config.MD](src/config.MD) for its fields and defaults.

## Testing

`tests/crc.rs` checks the CRC-32 implementation against known values,
using only the public `crc32` function - a `#[cfg(test)]` unit test in
`src/` is reserved for the rare case that needs access to something that
should stay private (see CLAUDE.md's testing section), which `crc32`
doesn't. `tests/smoke.rs` is the minimum-viable check for a foundational
crate: it compiles and its main types construct. Run just this crate
with:

```sh
cargo test -p common
```
