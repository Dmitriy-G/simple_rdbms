# readiness

`server::health::Readiness`'s state transitions, exercised entirely
through its public API (`Readiness`, `ReadinessState`, `state()`).
Formerly an inline `#[cfg(test)] mod tests` in `crates/server/src/health.rs`;
moved here once `server` gained a `[lib]` target (see
`crates/server/src/lib.MD`) made it reachable from `tests/`, per
CLAUDE.md's testing section - an inline `#[cfg(test)]` module in `src/` is
only justified when a test needs access to something that should stay
private, and nothing here does.

## Key Components

- `starts_not_ready` - a freshly constructed `Readiness` reports
  `ReadinessState::Starting`.
- `set_ready_reports_ready` - `set_ready` transitions to
  `ReadinessState::Ready`.
- `set_not_ready_reports_shutting_down_once_ready` - `set_not_ready`,
  called after `set_ready`, transitions to
  `ReadinessState::ShuttingDown` rather than back to `Starting`.
- `state_names_are_distinct` - each `ReadinessState` variant's `Display`
  output matches the lowercase, `snake_case` name `/health/ready`'s
  response body uses (`"starting"`, `"ready"`, `"shutting_down"`).

## Usage Example

```rust
use server::health::{Readiness, ReadinessState};

let readiness = Readiness::new();
assert_eq!(readiness.state(), ReadinessState::Starting);
readiness.set_ready();
assert_eq!(readiness.state(), ReadinessState::Ready);
```

Run via `cargo test -p server --test readiness`, alongside
`tests/endpoints.rs`'s HTTP-level check of the same states - this file
checks the state machine in isolation, without a listener or a real
connection involved.
