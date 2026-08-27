# send_sync

Compile-time proof that M10.1's storage-layer concurrency refactor
(`docs/ROADMAP.md`) actually delivers what it promises: every long-lived
type the engine shares across statement execution - the buffer pool, the
disk manager, the write-ahead log, the double-write buffer, the catalog,
the transaction manager - is `Send + Sync`, with no remaining
`Rc`/`RefCell`/`Cell` anywhere in reach. Lives here rather than in
`crates/storage/tests/` because two of the six types (`Catalog`,
`TransactionManager`) live in crates `storage` is not allowed to depend
on even as a dev-dependency (`storage -> common, types` per
`CLAUDE.md`'s crate-splitting rule); `engine` is the lowest crate in the
workspace with a legitimate dependency edge to all six.

## Key Components

- `assert_send_sync<T: Send + Sync>()` - private, generic, and never
  called with its type parameter inferred from a value: instantiating it
  explicitly (`assert_send_sync::<BufferPool>()`) is itself the
  assertion. A type that is not `Send + Sync` fails to compile here,
  which is what makes this a compile-time check rather than a runtime
  one - there is nothing to assert at runtime once the crate has
  compiled.
- `storage_types_are_send_and_sync` - instantiates `assert_send_sync`
  for `BufferPool`, `DiskManager`, `LogManager`, `DoubleWriteBuffer`,
  `Catalog`, and `TransactionManager`. Passes (compiles) or the build
  fails; there is no assertion body to run.

## Usage Example

```rust
fn assert_send_sync<T: Send + Sync>() {}

#[test]
fn storage_types_are_send_and_sync() {
    assert_send_sync::<storage::buffer::BufferPool>();
}
```

`cargo test --workspace` compiles this file as part of the ordinary test
run; a future change that reintroduces `Rc`/`RefCell`/`Cell` into any of
these six types' reachable fields fails the build right here, before it
can silently reintroduce the type-level barrier to concurrency M10.1
removed.
