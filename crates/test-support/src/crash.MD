# crash

The crash-injection sweep algorithm, generalized out of what used to be
two independent copies: `crates/storage/tests/btree_crash_injection.rs`'s
(drives a raw `storage::btree::BTreeIndex` and checks recovered rid sets)
and `crates/engine/tests/crash_injection.rs`'s (drives SQL through an
`engine::Database` and checks recovered table rows). CLAUDE.md calls this
pair "the repository's primary correctness gate for durability" - the
risk T-2 (`task.MD`) named directly: two copies of the same sweep mean a
fix to one can silently miss the other. The two originals differ in
handle type, workload-item type, and state type at every step, so the
generalization is a trait (`CrashWorkload`) the two call sites each
implement locally against their own domain types, rather than a version
of either original rewritten to serve both - `test-support` depends on
`storage` but not `engine`, so an `engine`-specific impl could not live
here even if that were otherwise desirable.

The sweep itself - the part that actually was duplicated - is exactly
this, unchanged in every phase in both originals: run the workload once
under a fault-free `storage::block_device::DurabilityModel` to learn the
total number of device writes it performs; then, for every write index
`1..=total`, inject a fault there, drive the workload until it stops
(the "safe prefix"), make two more best-effort recovery attempts that
themselves inject an early fault (proving recovery-during-recovery cannot
corrupt further), then recover cleanly and assert the recovered state
matches replaying exactly the safe prefix from scratch.

## Key Components

- `CrashWorkload` - the trait each concrete workload implements.
  `len(&self) -> usize` - how many workload items there are, for the
  fault-free run's completeness check. `open(&self, dir, devices) ->
  Result<Self::Handle, _>` - opens (and, for a real crash, recovers) a
  live handle from a `DeviceTriple` (`devices.MD`); `Self::Handle` is
  whatever type the workload actually drives (`storage::buffer::BufferPool`
  for the B+tree workload, `engine::Database` for the SQL workload).
  `drive(&self, handle) -> usize` - runs the workload against `handle`
  until the first error, returning how many leading items are durably
  safe; called both for the fault-free total-writes measurement and for
  every faulted sweep point, since "run until a crash" and "run to
  completion" are the same loop with a fault only sometimes present.
  `expected_state(&self, safe_prefix) -> Result<Self::State, _>` -
  computes (or, where computing directly isn't possible, replays) the
  state that ought to survive if exactly the leading `safe_prefix` items
  are durable. `observed_state(&self, safe_prefix, handle) -> Result<Self::State,
  _>` - extracts the comparable state a cleanly recovered `handle` actually
  has; also where a workload verifies its own structural invariants (e.g.
  the B+tree impl's `check_invariants` call) before returning, since a
  structural failure surfaces more clearly as a `observed_state` error than
  as a silent state mismatch. `Self::State: PartialEq + Debug` so the sweep
  can `assert_eq!` it directly.
- `assert_workload_is_crash_safe(workload, model) -> Result<(), _>` - the
  sweep described above, generic over any `CrashWorkload`. Every `dir` is
  a fresh `tempfile::tempdir()` per sweep point, matching both originals'
  isolation (no state leaks between simulated crashes). The fault-free
  first phase only asserts `total_writes > 0`, not that `drive` returns
  `item_count()` - the two are not the same thing for every workload: the
  SQL workload's `mid_transaction_kill` deliberately ends with a `BEGIN`/
  `INSERT` that never commits, so every statement can return `Ok` (nothing
  errors) while the safe prefix `drive` returns is still shorter than
  `item_count()`. An earlier version of this function asserted the two
  were equal and failed that workload's fault-free run outright; the
  fix is that `total_writes` comes from the shared write counter, never
  from `drive`'s return value, so nothing about computing it actually
  depends on every item committing.

## Usage Example

```rust
use test_support::{CrashWorkload, assert_workload_is_crash_safe};

struct MyWorkload { /* whatever the domain needs */ }

impl CrashWorkload for MyWorkload {
    type Handle = MyHandle;
    type State = MyComparableState;

    fn len(&self) -> usize { /* ... */ }
    fn open(&self, dir: &std::path::Path, devices: test_support::DeviceTriple)
        -> Result<Self::Handle, Box<dyn std::error::Error>> { /* ... */ }
    fn drive(&self, handle: &mut Self::Handle) -> usize { /* ... */ }
    fn expected_state(&self, safe_prefix: usize)
        -> Result<Self::State, Box<dyn std::error::Error>> { /* ... */ }
    fn observed_state(&self, safe_prefix: usize, handle: &mut Self::Handle)
        -> Result<Self::State, Box<dyn std::error::Error>> { /* ... */ }
}

assert_workload_is_crash_safe(&MyWorkload { /* ... */ }, DurabilityModel::torn_write())?;
```

`crates/storage/tests/btree_crash_injection.rs` and
`crates/engine/tests/crash_injection.rs` each define one private
`CrashWorkload` impl and call this function once per durability model per
workload variant - four models times however many workloads each file
defines, exactly as many test functions as before, now sharing one sweep
implementation instead of two.
