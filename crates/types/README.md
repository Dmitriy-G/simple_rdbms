# types

The value system shared by the SQL layer, catalog, and storage engine:
column types, runtime values, and the tuple encoding every layer that
touches a row agrees on.

## Architecture

`types` sits just above `common` in the layered workspace (see
`docs/adr/0002-crate-splitting.md`), depending on nothing else. Every crate
that needs to talk about a row's shape or contents — `catalog`, `sql`,
`planner`, `executor`, and `storage`'s callers — shares this one vocabulary
instead of each defining its own, so a `Value` decoded out of a heap page
in `storage` is the exact same type a `WHERE` clause in `executor` compares
against.

## Key Components

- `data_type` - `DataType`, the set of column types the engine understands.
  See [data_type.MD](src/data_type.MD).
- `value` - `Value`, a runtime value: one cell of a tuple. See
  [value.MD](src/value.MD).
- `tuple` - `Tuple`, `Encode`, `Decode`, `TupleError`: an ordered row of
  values, the traits used to convert one to and from on-disk bytes, and the
  error that conversion can raise. See [tuple.MD](src/tuple.MD).

## Features

The full `DataType`/`Value` set this codebase currently uses (integers,
text, and their `NULL` handling) round-trips through `Tuple::encode`/
`Tuple::decode` today. There is no fixed-point/decimal, date/time, or
binary blob type yet — those would be additions to `DataType`/`Value`
rather than a stubbed method, so there's no `todo!()` marking their
absence; they simply aren't variants yet.

## Dependencies

Workspace: `common`, for `Result` and the newtype ids a `Value` can carry
(e.g. `Rid` is not part of this crate, but ids like it come from `common`).
External: `thiserror`, for `TupleError`. Dev-only: `proptest`, for the
round-trip property test in `tests/tuple_roundtrip.rs`.

## Configuration

None — `types` has no configuration of its own.

## Testing

There are no inline `#[cfg(test)]` unit tests; everything lives under
`tests/`. `tests/tuple_roundtrip.rs` is a `proptest`-driven property test asserting
`Tuple::decode` reconstructs exactly what `Encode::encode` produced, for any
schema and matching set of values. `tests/smoke.rs` is the minimum-viable
compile-and-construct check. Run just this crate with:

```sh
cargo test -p types
```
