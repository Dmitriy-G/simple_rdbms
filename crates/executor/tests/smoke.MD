# smoke

Asserts the crate compiles and its main types can be constructed.

## Key Components

- `seq_scan_executor_constructs_as_trait_object` - builds a
  `SeqScanExecutor` and checks it coerces into a `Box<dyn Executor>`.

## Usage Example

```rust
#[test]
fn example_constructs_a_scan() {
    let scan = SeqScanExecutor::new(common::TableId(1));
    let _boxed: Box<dyn executor::Executor> = Box::new(scan);
}
```

Run as part of `cargo test --workspace`; catches a broken build or a
signature change in `Executor`/`SeqScanExecutor` before it surfaces
indirectly in a downstream crate's tests.
