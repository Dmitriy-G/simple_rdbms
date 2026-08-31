# logging

A thread-local tracing capture rig, moved here from what used to be three
independent copies: `crates/engine/tests/support/mod.rs`,
`crates/storage/tests/recovery_logging.rs` (an inline copy that never went
through any `mod support;`), and effectively re-derived a third time by
`crates/engine/tests/sessions.rs` before that file moved off log capture
entirely in favor of a real counter (`crates/engine/tests/sessions.MD`).
Every field and method is unchanged from the originals; only the
`#[cfg(test)]` gates on `impl Write for CaptureBuf` and `captured_events`
are gone - they were only ever meaningful inside a `tests/*.rs` integration
test binary (itself compiled with `--cfg test`), and every caller of this
crate is exactly that kind of binary, so the gate never actually excluded
anything and is simpler dropped than carried forward for a crate that has
no other build configuration to distinguish.

## Key Components

- `CaptureBuf` - a cheaply-cloneable, `Send`-safe in-memory buffer
  (`Arc<Mutex<Vec<u8>>>`) implementing `std::io::Write`, so a
  `tracing_subscriber` writer closure can hand out clones of the same
  underlying buffer to every event it formats.
- `captured_events(buf) -> Vec<serde_json::Value>` - parses every
  non-empty line written into a `CaptureBuf` as one JSON object
  (`tracing_subscriber`'s `.json()` formatter emits one event per line)
  and returns them in emission order, for a test to search or filter over.
- `set_capturing_subscriber(capture) -> tracing::subscriber::DefaultGuard` -
  installs a JSON-formatting subscriber at `TRACE` that writes every event
  into a clone of `capture`, as the thread-local default for as long as
  the returned guard is alive. Thread-local rather than global
  (`tracing::subscriber::set_default`, not `set_global_default`) so
  concurrently-running tests in the same binary don't fight over one
  subscriber - though see `crates/engine/tests/sessions.MD`'s note on
  `checkpoint_count` for why thread-local capture is not, on its own,
  a substitute for asserting behavior through a real counter when a test
  runs alongside others: tracing's callsite-interest cache is
  process-wide, not per-thread.

## Usage Example

```rust
use test_support::{CaptureBuf, captured_events, set_capturing_subscriber};

let capture = CaptureBuf::default();
let _guard = set_capturing_subscriber(&capture);

let mut db = engine::Database::open(config)?;
db.execute("CREATE TABLE t (a INTEGER)")?;

let events = captured_events(&capture);
assert!(events.iter().any(|event| event["fields"]["message"] == "database opened"));
```

`crates/engine/tests/logging.rs` installs the subscriber before
`engine::Database::open`, since that is what spawns `engine`'s dedicated
engine thread and captures whatever dispatcher is active at that moment
(`crates/engine/src/runtime.MD`'s "Propagating the tracing subscriber
across the thread boundary") - call order matters here regardless of
which crate `CaptureBuf` itself lives in.
