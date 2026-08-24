# bench_fetch_page

A manual micro-benchmark for `BufferPool::fetch_page`'s hot path, run with
tracing effectively disabled (no subscriber installed - see
CLAUDE.md's logging section on hot-path discipline: a `trace!` call with
no subscriber listening compiles down to a single level check, which is
the case this benchmark measures). `#[ignore]`d rather than part of
`cargo test --workspace`: CLAUDE.md's testing rules forbid wall-clock
assertions in the deterministic suite, and a benchmark has nothing to
assert against anyway - it exists to be read by a human, not to pass or
fail.

There is no automated "before" to compare against, since the `trace!`
call already exists in the same commit that adds this benchmark; what
this pins down instead is a current baseline a later change can be
compared to if `fetch_page`'s hot path is ever suspected of regressing.

## Key Components

- `fetch_page_hot_loop_is_unaffected_by_disabled_trace_calls` - allocates
  one page, then fetches it 200,000 times from an already-resident buffer
  pool frame (no disk I/O after the first call), timing the loop and
  printing nanoseconds per call. Measured at ~41 ns/call in release mode
  on the machine this was written on - fetching an already-pinned-once,
  now-resident page is dominated by the `RefCell`/`HashMap` bookkeeping in
  `BufferPool::fetch_page`/`pin`, not by the `trace!` call added in the
  same change.

## Usage Example

```sh
cargo test -p storage --release --test bench_fetch_page -- --ignored --nocapture
```

Not run by CI or `cargo test --workspace` - a human runs it manually
(always in `--release`; debug-mode timings aren't representative) when
touching `BufferPool::fetch_page` or anything on its call path, to check
the ns/call figure hasn't moved by more than noise.
