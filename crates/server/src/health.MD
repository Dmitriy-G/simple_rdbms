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

## Key Components

- `Readiness` - a cheap, `Clone`able handle (an `Arc<AtomicBool>`
  underneath) shared between `main`'s startup sequence and the HTTP
  listener thread. Starts `false` - reporting the `Starting` state from
  the moment the process comes up - and is flipped exactly once, by
  `main` right after `Database::open` returns successfully.
- `Readiness::new() -> Self` - starts not-ready.
- `Readiness::set_ready(&self)` - marks the database open and recovered.
  Never called again after that, and never un-set - there is no code path
  in this binary that keeps running after the database it opened stops
  being usable.
- `Readiness::is_ready(&self) -> bool` - read by `http::serve` on every
  `/health/ready` request.

## Usage Example

```rust
let readiness = Readiness::new();
assert!(!readiness.is_ready());
readiness.set_ready();
assert!(readiness.is_ready());
```

`main` constructs one `Readiness`, clones it into the HTTP listener thread
before that thread starts accepting connections, and calls `set_ready`
once `Database::open` (which includes crash recovery) has returned - see
`main.MD` for the exact ordering that makes this safe.
