# mod

Shared test-only helpers for `engine`'s own `tests/` integration suite,
declared with `mod support;` by whichever test file needs them. Nothing
here is gated behind `#[cfg(test)]` in `src/` - that attribute only takes
effect when compiling the crate's own unit-test binary, so it is invisible
to `tests/`, which links against an ordinary (non-`--cfg test`) build of
the crate; see CLAUDE.md's testing section for when `#[cfg(test)]` in
`src/` is still the right call (only when the test needs access to a
private item), and `storage/tests/support/mod.MD` for the same pattern
used by another crate's suite - `mod support;` only resolves within one
crate's `tests/` directory, so it cannot be shared across crates, which is
why each crate that needs it carries its own copy.

## Key Components

- `CaptureBuf` - a cheaply-cloneable, `Send`-safe in-memory buffer
  (`Arc<Mutex<Vec<u8>>>`) implementing `std::io::Write`, so a
  `tracing_subscriber` writer closure can hand out clones of the same
  underlying buffer to every event it formats. Originally private to
  `logging.rs`; moved here once `sessions.rs` needed the identical
  pattern to observe checkpoint completion events, per the sharing rule
  above.
- `captured_events(buf) -> Vec<serde_json::Value>` - parses every non-empty
  line written into a `CaptureBuf` as one JSON object (`tracing_subscriber`'s
  `.json()` formatter emits one event per line) and returns them in
  emission order, for a test to search or filter over.
- `set_capturing_subscriber(capture) -> tracing::subscriber::DefaultGuard` -
  installs a JSON-formatting subscriber at `TRACE` that writes every event
  into a clone of `capture`, as the thread-local default for as long as the
  returned guard is alive. Thread-local rather than global
  (`tracing::subscriber::set_default`, not `set_global_default`) so
  concurrently-running tests in the same binary don't fight over one
  subscriber; must be called *before* `engine::Database::open`, since that
  is what spawns the engine thread and captures whatever dispatcher is
  active at that moment (`runtime.MD`'s "Propagating the tracing subscriber
  across the thread boundary").

## Usage Example

```rust
mod support;
use support::{CaptureBuf, captured_events, set_capturing_subscriber};

let capture = CaptureBuf::default();
let _guard = set_capturing_subscriber(&capture);

let mut db = engine::Database::open(config)?;
db.execute("CREATE TABLE t (a INTEGER)")?;

let events = captured_events(&capture);
assert!(events.iter().any(|event| event["fields"]["message"] == "database opened"));
```

`crates/engine/tests/logging.rs` and `crates/engine/tests/sessions.rs` both
declare `mod support;` and drive this exact sequence - install the
subscriber, open (or connect another session against) a `Database`, then
inspect `captured_events` for the specific fields under test - so a
capturing subscriber only ever sees events from the one test that installed
it, not from tests running in other threads of the same binary.
