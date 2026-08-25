# health

Liveness and readiness are kept as two separate questions on purpose (see
`docs/ROADMAP.md`'s M13 entry and CLAUDE.md): the process being alive and
the process being able to usefully do anything are not the same fact.
ARIES recovery on a large log can take minutes; during that window the
process is alive (its metrics/health listener answers) but not ready (it
cannot yet do the work `Database::open` hasn't finished). A single
combined check would either have Docker kill the container mid-recovery
(if it required readiness) or route traffic into a database that can't
serve it yet (if it only checked liveness). `Readiness` is the one bit
this crate needs to tell those two questions apart; liveness itself needs
no state at all - `http::serve` answers `/health/live` as soon as it's
running, regardless of `Readiness`.

Readiness is not a one-way flag: a container that is checkpointing and
closing its database on the way down is exactly as unable to serve a
statement as one that hasn't finished recovering yet, so `/health/ready`
must be able to go false a second time. It is deliberately not the *same*
false as startup, though - "still recovering" and "shutting down" call for
different operator reactions to a restart loop (wait longer, vs. this
container is draining on purpose), so `Readiness` tracks a three-state
`ReadinessState` rather than a boolean, and `/health/ready` echoes the
state's name in its response body.

## Key Components

- `ReadinessState` - `Starting` | `Ready` | `ShuttingDown`. `Display`
  gives each a lowercase, `snake_case` name (`"starting"`, `"ready"`,
  `"shutting_down"`) suitable for a response body or a log field.
- `Readiness` - a cheap, `Clone`able handle (an `Arc<AtomicU8>`
  underneath, storing a `ReadinessState`) shared between `main`'s startup
  sequence and the HTTP listener thread.
- `Readiness::new() -> Self` - starts in `ReadinessState::Starting`.
- `Readiness::set_ready(&self)` - transitions to `ReadinessState::Ready`;
  called by `main` right after `Database::open` returns successfully.
- `Readiness::set_not_ready(&self)` - transitions to
  `ReadinessState::ShuttingDown`; called by `main` as the first action
  once the shutdown signal arrives, before `Database::close` runs, so
  `/health/ready` stops claiming the container can serve a statement for
  the entire duration of the checkpoint-and-close sequence, not just
  after it completes.
- `Readiness::state(&self) -> ReadinessState` - read by `http::serve` on
  every `/health/ready` request; the response is `200` with body
  `"ready\n"` only in `ReadinessState::Ready`, and `503` with the state's
  own `Display` name as the body otherwise - see `http.MD`.

## Usage Example

```rust
let readiness = Readiness::new();
assert_eq!(readiness.state(), ReadinessState::Starting);

readiness.set_ready();
assert_eq!(readiness.state(), ReadinessState::Ready);

readiness.set_not_ready();
assert_eq!(readiness.state(), ReadinessState::ShuttingDown);
```

`main` constructs one `Readiness`, clones it into the HTTP listener thread
before that thread starts accepting connections, calls `set_ready` once
`Database::open` (which includes crash recovery) has returned, and calls
`set_not_ready` the instant the shutdown signal arrives - see `main.MD`
for the exact ordering that makes both transitions safe.
