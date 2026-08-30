# dwb_batch_exclusion

Proves `buffer::BufferPool::flush_pages`'s `flush_sequence` mutex
(`crates/storage/src/buffer.MD`'s flush-sequence rule) actually serializes
the double-write protocol across concurrent flushes, rather than trusting
the counter-only assertions `buffer_pool_concurrency.rs`'s
`concurrent_read_modify_write_cycles_never_lose_an_update` makes. That
test passes even when two flushes interleave their `write_batch`/
`clear_batch` calls, because nothing crashes mid-sequence and it only
checks final counter values; this file asserts the interleaving invariant
directly, by watching the bytes the double-write buffer itself writes,
instead of waiting for a crash to turn the gap into a symptom.

## Key Components

- `HEADER_COUNT_OFFSET` - `8`, the byte offset of the double-write
  buffer's entry count within its header page
  (`crate::dwb`'s private `MAGIC`/`ENTRY_COUNT_OFFSET` layout, mirrored
  here rather than exposed, since a test-only device has no need to touch
  the rest of the crate's internals).
- `BatchNestingDevice` - wraps an inner `BlockDevice` (the double-write
  buffer's real file, opened directly rather than through
  `DoubleWriteBuffer::open`, so a `BatchNestingDevice` can sit between it
  and `DoubleWriteBuffer::open_with_device`) and inspects every write to
  offset `0` - the header `write_batch`/`clear_batch` both rewrite -
  without altering it. A header whose count is nonzero means a batch is
  now outstanding; a zero count means one just closed. `outstanding` (an
  `AtomicBool`) tracks the currently-believed state via `swap`: opening
  while already `true`, or closing while already `false`, means two
  batches' lifetimes overlapped on this device, and increments
  `violations` (an `AtomicUsize`) accordingly. `batches_opened` counts
  every open, independent of correctness, so the test can assert the
  scenario it wants (a real flush happening at all) was actually
  exercised.
- `RMW_PAGE_COUNT` / `RMW_THREAD_COUNT` / `RMW_POOL_SIZE` / `RMW_ROUNDS` -
  the same shape as `buffer_pool_concurrency.rs`'s read-modify-write
  constants (3 pages, 8 threads, a 4-frame pool, `RMW_ROUNDS` rounds):
  more threads than pages and a pool too small for all of them forces
  `acquire_free_frame` to evict, and eviction is `flush_pages`'s only
  concurrent entry point among ordinary callers
  (`flush_page`/`flush_all` are otherwise always driven by one thread at
  a time in these tests). `RMW_ROUNDS` is smaller than the read-modify-write
  test's own count, since this test only needs `batches_opened > 0` and a
  clean pass, not a specific number of surviving increments.
- `concurrent_flushes_never_interleave_a_double_write_batch` - builds a
  `BufferPool` whose `DoubleWriteBuffer` is opened over a
  `BatchNestingDevice`, allocates `RMW_PAGE_COUNT` pages and flushes them,
  then runs the same forced-eviction-then-barrier-released-writers shape
  `concurrent_read_modify_write_cycles_never_lose_an_update` uses for
  `RMW_ROUNDS` rounds, so that multiple threads' `PageWriteGuard::write`
  calls dirty pages that a subsequent `acquire_free_frame` in another
  thread evicts and flushes while this round's or the next round's
  writers are still active. A final `flush_all` catches anything left
  dirty. Asserts `violations == 0` and `batches_opened > 0` - the second
  assertion is what keeps this test honest, since a build that stopped
  flushing entirely would otherwise satisfy the first vacuously. Without
  `buffer::BufferPool`'s `flush_sequence` mutex, this reliably reports at
  least one violation within the configured rounds; with it, the whole
  `write_batch` -> real writes -> `sync` -> `clear_batch` sequence is
  exclusive, so no second flush can ever observe (or produce) a header
  transition out of order.

## Usage Example

```rust
let device = BatchNestingDevice {
    inner: Box::new(FileDevice::new(dwb_file)),
    outstanding: Arc::clone(&outstanding),
    violations: Arc::clone(&violations),
    batches_opened: Arc::clone(&batches_opened),
};
let dwb = DoubleWriteBuffer::open_with_device(Box::new(device), DoubleWriteBuffer::DEFAULT_CAPACITY)?;
let pool = BufferPool::new(disk, dwb, log, RMW_POOL_SIZE, replacer);
// ... drive concurrent flushes through `pool` ...
assert_eq!(violations.load(Ordering::SeqCst), 0);
```

Run as part of `cargo test --workspace` like any other `crates/storage/tests`
file; unlike `buffer_pool_concurrency.rs` it needs no dedicated
`--test-threads=1` CI step, since its assertion is a direct invariant
check on the bytes written rather than a timing-sensitive race it is
merely likely to hit - `flush_sequence` either serializes every observed
header transition or it does not, regardless of how many other test
binaries' threads are competing for cores at the same time.
