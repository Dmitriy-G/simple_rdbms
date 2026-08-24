# executor

A single node in a pull-based (Volcano-model) operator tree. Every
physical plan node lowers to one `Executor`; a parent operator calls
`next` on its children to pull their output one tuple at a time, rather
than any operator materializing its whole result up front.

## Key Components

- `Executor` - trait with `init(&mut self, ctx) -> Result<(), ExecutorError>`,
  which prepares the executor to produce tuples (opens iterators,
  evaluates setup work, and recursively initializes any child executors)
  and must be called exactly once before the first call to `next`, and
  `next(&mut self, ctx) -> Result<Option<Tuple>, ExecutorError>`, which
  pulls the next output tuple or returns `Ok(None)` once the operator is
  exhausted and must not be called again afterward.

## Usage Example

```rust
use executor::Executor;

executor.init(&mut ctx)?;
while let Some(tuple) = executor.next(&mut ctx)? {
    // consume tuple
}
```

Every operator in `operators/` implements this trait by delegating to its
children's `init`/`next` and doing its own per-node work in between (e.g.
`FilterExecutor::next` loops calling `child.next` until a row passes the
predicate); `engine::Database::run` drives the root of the tree the same
way, with no special-casing per operator kind.
