# smoke

Asserts the crate compiles and its main types can be constructed.

## Key Components

- `binder_and_optimizer_construct` - builds a `Binder` over an empty
  `Catalog` and an `Optimizer` with no rules, checking neither panics to
  construct.
- `plan_nodes_construct` - builds a `LogicalPlan::SeqScan` and a
  `PhysicalPlan::SeqScan` and checks each matches its own variant.

## Usage Example

```rust
#[test]
fn example_constructs_a_binder() {
    let catalog = catalog::Catalog::new();
    let _binder = planner::Binder::new(&catalog);
}
```

Run as part of `cargo test --workspace`; catches a broken build or a
signature change in `Binder`/`Optimizer`/`LogicalPlan`/`PhysicalPlan`
before it surfaces indirectly in a downstream crate's tests.
