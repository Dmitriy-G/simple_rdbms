# worker_pool

A small, fixed-size thread pool that runs the actual work of every
`EngineMessage::Execute` off the engine's own dispatch thread
(`runtime.MD`). M10.2 (`docs/ROADMAP.md`) replaces the single-transaction
admission control the engine used to enforce with real concurrent
statement execution: several sessions' statements now run at the same
time, each on its own worker thread, while the dispatch thread stays free
to keep accepting `Connect`/`Disconnect`/`Execute` messages and running
its periodic tick. This module owns nothing about SQL or sessions - it is
a generic "run this closure on some worker thread" primitive, kept
separate from `runtime.rs` so that thread-pool bookkeeping (the job
channel, spawning, shutdown) doesn't get tangled up with the
session/transaction bookkeeping that actually decides what to run.

## Key Components

- `Job` - `pub(crate)` type alias for `Box<dyn FnOnce() + Send + 'static>`,
  one unit of work submitted to the pool. `runtime.rs`'s `dispatch_execute`
  is the only place that builds one today: a closure that locks the
  target session, calls `EngineShared::execute`, and replies.
- `WorkerPool` - `pub(crate)`. `sender: Option<mpsc::Sender<Job>>` (the
  submission side of the job queue; `Option` only so `Drop` can `take()`
  it) plus `workers: Vec<JoinHandle<()>>`. `WorkerPool::new(size, dispatch)
  -> io::Result<Self>` - spawns `size` OS threads, each running
  `worker_loop` against a shared `Arc<Mutex<mpsc::Receiver<Job>>>` (a
  `Receiver` has no built-in multi-consumer support, so every worker takes
  the same mutex just long enough to pull the next job off the channel,
  never while running it); `dispatch` is cloned once per worker so every
  worker thread emits through the same subscriber the engine thread itself
  uses (see `runtime.MD`'s "Propagating the tracing subscriber across the
  thread boundary" for why this matters - the same reasoning applies to
  every additional thread this pool adds). Returns the first thread-spawn
  `io::Error` if one fails partway through, exactly as `EngineHandle::spawn`
  already does for the engine thread itself; a worker already spawned
  before the failure keeps running until its channel disconnects (the
  partially-built `sender`/`receiver` locals drop at the `?`, and once
  every clone of `sender` is gone, that worker's next `recv()` sees
  `Err` and exits on its own).
  `WorkerPool::submit(&self, job)` - pushes a job onto the channel;
  submitting after every worker has stopped (which cannot happen while any
  `WorkerPool` is alive, since it owns `sender`) would just be a silently
  dropped send, matching how every other channel send in this crate
  degrades rather than panics.
- `worker_loop(receiver, dispatch)` - private free function, one worker
  thread's entire body. Wraps the whole loop in
  `tracing::dispatcher::with_default(dispatch, ...)`, then repeatedly locks
  `receiver` just to pull the next `Job` and drops the lock before running
  it - a job can take arbitrarily long (a full table scan, a slow
  `CREATE INDEX`), and holding the queue's own mutex across that would
  turn every other worker thread's `recv()` into a wait on this one job,
  which defeats the entire point of having more than one worker. Exits
  when `recv()` returns `Err` (the channel disconnected - every `Sender`
  clone, meaning the pool's own `sender` field, has been dropped).
- `impl Drop for WorkerPool` - `take()`s `sender` (disconnecting the
  channel; no worker's `recv()` can succeed again after this) and then
  joins every worker thread, so a `WorkerPool` going out of scope - which
  happens when `runtime.rs`'s per-thread `EngineState` drops at the end of
  `run_engine`'s loop - never leaves a worker thread running past the
  engine thread's own shutdown, the same guarantee `EngineHandle::open`'s
  `impl Drop for EngineHandle` already gives for the engine thread itself.

## Usage Example

```rust
let dispatch = tracing::dispatcher::get_default(|d| d.clone());
let pool = WorkerPool::new(8, dispatch)?;
let (reply, reply_rx) = std::sync::mpsc::sync_channel(1);
pool.submit(Box::new(move || {
    let _ = reply.send(42);
}));
assert_eq!(reply_rx.recv().unwrap(), 42);
```

`runtime.rs`'s `EngineHandle::spawn` builds one `WorkerPool` alongside the
engine's shared state and moves both into the engine thread; every
`EngineMessage::Execute` the dispatch loop receives afterward turns into
exactly one `submit` call rather than running inline, which is what lets
two sessions' statements actually execute at the same time instead of
only being *admitted* concurrently while still running one at a time on a
single thread.
