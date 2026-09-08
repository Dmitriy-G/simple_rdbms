# dispatch_never_blocks

Regression coverage for P-14 (found reviewing M10.2 subtask 1): the
dispatch thread in `crates/engine/src/runtime.rs` must
never block on a lock a worker thread might be holding for a statement's
whole duration, since that turns one slow session's `SELECT` into a
stall for every other connected session. Unlike `tests/sessions.rs`,
which drives its concurrency through real wall-clock sleeps, this file
uses a device-level pause so the "long-running `SELECT`" in its test is
genuinely stuck - not just slow - for as long as the test wants, which is
what lets it assert responsiveness without a timing-dependent race: if
the dispatch thread were still blocking on the held lock, the probe
request below would hang until the pause is released rather than merely
arrive a bit late.

It now also covers P-53/P-55, which is the same property one layer down:
the catalog itself must never hold a lock that a statement's length can
stretch. **The requirement pulls against `catalog_reload_race.MD`'s**, and
the two files have to be read together - that suite needs a reload to
exclude every concurrent catalog mutation, and the first attempt at giving
it that excluded readers too, which is precisely the stall this file
asserts against.
`docs/adr/0014-catalog-is-a-live-shared-object.md` is the design that
satisfies both: a mutation `Mutex` for `catalog_reload_race`, and a state
`RwLock` held only for one lookup for this file. Break either apart and
one of the two suites fails; which one it is tells you which half was
broken.

It now also covers P-54: `EngineMessage::Checkpoint`/`BestEffortFlush`
used to run `EngineShared::checkpoint_and_flush`/`best_effort_flush`
inline on the dispatch thread despite both doing real I/O (a page flush),
so a stalled flush froze every other session's `Connect`/`Execute` the
same way a stalled lock did for P-14. `dispatch_checkpoint`/
`dispatch_best_effort_flush` (`runtime.MD`) now submit that work to the
worker pool instead, and
`a_stalled_checkpoint_does_not_block_an_unrelated_sessions_dispatch`
below is the regression test, pausing a *write* rather than a read since
that is where a checkpoint's flush actually blocks.

**What that test asserts, and what it deliberately does not.** Its probe
is a `Connect` plus a `table_names()`, both served inline by the dispatch
thread, because those are exactly the requests P-54 makes immune to a
stalled checkpoint. A probe that ran a real statement would *not* pass,
and not because the dispatch thread is blocked: a checkpoint holds
`EngineShared.txn_manager` across `write_checkpoint`'s own page-0 flush
(`crates/txn/src/checkpoint.rs`, `flush_page(PageId(0))` under the lock
`EngineShared::checkpoint_and_flush` took), and every statement needs that
same mutex to begin its transaction
(`EngineShared::txn_for_statement`). So a stalled checkpoint still stalls
statements — through a data structure's lock, not through the dispatch
thread — and closing that gap is a separate change to the checkpoint's
locking, tracked as its own problem entry. Widening this test's timeout
until an `Execute` probe passes is therefore not an option: the wait it
would be tolerating is unbounded.

## Key Components

- `REACHED_TIMEOUT`/`RESPONSIVENESS_BOUND`/`PAUSE_SAFETY_NET` - the three
  bounds the test's blocking waits are held to, per `CLAUDE.md`'s
  concurrency-test discipline: `REACHED_TIMEOUT` bounds waiting for the
  paused `SELECT` to actually reach its blocked read (generous - this is
  just proving setup worked, not the property under test),
  `RESPONSIVENESS_BOUND` is the actual assertion (tight - a regression
  that reintroduces dispatch-thread blocking must fail this, not merely
  slow it down), and `PAUSE_SAFETY_NET` bounds the device's own internal
  wait so a broken test can't hang the suite forever even if the test
  function itself never reaches its release send. Both tests share all
  three: each probes with in-memory dispatch-thread work only, so
  `RESPONSIVENESS_BOUND` is the right order of magnitude for either, and
  a bound that has to be widened to pass is the signal that the probe is
  waiting on a lock rather than on a scheduler.
- `PauseSlot` - one arming's worth of synchronization: the `reached`
  `SyncSender` the device signals when it parks, and the `release`
  `Receiver` it then blocks on. The device holds a `VecDeque` of these and
  consumes one per arming, which is what lets a single test pause two
  different threads in sequence at two different reads.
- `PausingDevice` - wraps a real `FileDevice` and independently arms a
  read pause and a write pause, each its own `Arc<AtomicBool>` plus
  `Mutex`-wrapped `VecDeque<PauseSlot>` (`read_armed`/`read_slots`,
  `write_armed`/`write_slots`) so a test can pick which operation it needs
  to intercept without one pause's arming affecting the other.
  `PausingDevice::new(inner, read_armed, read_slots)` builds one with only
  read pausing live (write pausing disarmed and empty), for
  `a_paused_select_does_not_block_an_unrelated_sessions_request`'s device
  reads; `PausingDevice::with_write_pause(inner, write_armed, write_slots)`
  is the mirror image for
  `a_stalled_checkpoint_does_not_block_an_unrelated_sessions_dispatch`,
  which needs to intercept the page write a checkpoint's flush issues, not
  a read. `pause_if_armed(armed, slots)` is the shared body both `read_at`
  and `write_at` call: if armed (checked via `swap(false, ...)`, so only
  the *next* call after each arming pauses), it pops the front
  `PauseSlot`, signals that slot's `reached`, and blocks on its `release`
  (bounded by `PAUSE_SAFETY_NET`) before the caller delegates to the
  wrapped device. Every other `BlockDevice` method delegates straight
  through unpaused.
- `recv_within`/`row_count` - the same private helpers as
  `tests/sessions.rs` (`sessions.MD`), duplicated rather than shared
  since `tests/support/` is for helpers more than one file actually needs
  and this is currently the only other file using them.
- `a_paused_select_does_not_block_an_unrelated_sessions_request` - opens a
  database with a tiny `buffer_pool_size` (2 frames, so a page written
  during setup is evicted by the time the paused scan runs and really
  does need a disk read to intercept) through `PausingDevice`, creates a
  table spanning several pages, and commits unarmed so setup itself never
  pauses. Then: a second session (`db2`) opens an explicit transaction and
  is left idle; the device is armed; a third session (`db_scan`) starts
  `SELECT * FROM t` on its own thread, which blocks inside the paused read
  mid-statement; the test blocks on `scan_reached_rx` until it knows the
  scan is genuinely stuck there, not just about to run. The device is then
  **armed a second time** and `db2` is dropped, exercising the `Disconnect`
  path P-14 named: its `reload_catalog` runs on a worker, takes the
  catalog's mutation lock and parks in the second `PauseSlot`, and the test
  waits on `reload_reached_rx` before probing. That second wait is the
  point of the two-slot device - it makes the probe demonstrably run while
  a reload is *known* to be holding the catalog's mutation lock, where
  waiting only for the disconnect to be "in flight" left it a race whether
  the reload had reached the catalog at all. (Before ADR 0014 the suite
  passed for that reason and not on merit: with a two-frame pool the reload
  happened to block inside `Catalog::open` before ever reaching the write
  lock, so a faster open would have stalled the same probe.) A fourth
  session's `table_names()` is then sent on its own thread and must come
  back within `RESPONSIVENESS_BOUND` - proven by disabling the fix locally
  while
  writing this test: doing so made `disconnect` call
  `disconnect_session` inline instead of through `WorkerPool::submit`,
  and the probe request then hung past every bound in this file (in fact
  past the whole test binary's own default per-test timeout), because the
  dispatch thread was blocked behind the still-paused scan and never got to
  processing the probe's `Execute` message at all.
  Only after the probe request is confirmed to have returned does the test
  release both paused reads - the reload's first, then the scan's - and
  join the scan thread, asserting it still completed with the correct row
  count - the
  fix must not change what the paused statement itself sees, only stop it
  from starving every other session while it runs.
- `a_stalled_checkpoint_does_not_block_an_unrelated_sessions_dispatch` -
  opens a database through a `PausingDevice::with_write_pause` at the
  default `buffer_pool_size` (the pause is on a write this test forces,
  not on an eviction it has to engineer), creates a table and commits one
  row unarmed, opens a second session (`db_probe_base`) that stays alive
  across the whole test, then arms the write pause and calls
  `Database::close` (which sends `Checkpoint`) on its own thread. The
  first page write that checkpoint issues is page 0's header flush inside
  `write_checkpoint`; it parks there, and the test waits on
  `flush_reached_rx` until it knows the flush is genuinely stuck. Only
  then does it open a *third* session from `db_probe_base` and ask for
  `table_names()`, on its own thread, asserting the `Connect` and the
  lookup both come back within `RESPONSIVENESS_BOUND` and that the answer
  really contains `t` - `Database::table_names` returns an empty `Vec` on
  error, so asserting on the content is what separates "answered" from
  "failed fast". Before the P-54 fix, `dispatch_checkpoint` did not exist
  and `dispatch_message` ran `checkpoint_and_flush` to completion inline,
  so the dispatch thread was parked in the write and never reached the
  probe's `Connect` at all: the probe hung past every bound in this file,
  exactly as the P-14 probe did behind a stalled lock. The probe thread
  hands both its sessions back to the test rather than dropping them,
  because `Drop for Database` calls `best_effort_flush`, which would queue
  behind the very flush the test is holding; joining it is therefore done
  only after the release. Finally the pause is released and the checkpoint
  thread is joined on its `Result`, confirming the checkpoint completes
  rather than being abandoned mid-write.
- `wal_base_path(dir) -> PathBuf` - private helper, identical in shape to
  `tests/sessions.rs`'s helper of the same name, building the WAL's base
  path for `Database::open_with_devices`.

## Usage Example

```rust
let armed = Arc::new(AtomicBool::new(false));
let (reached_tx, reached_rx) = mpsc::sync_channel(0);
let (release_tx, release_rx) = mpsc::sync_channel(0);
let slots = VecDeque::from(vec![PauseSlot { reached: reached_tx, release: release_rx }]);
let device = PausingDevice::new(FileDevice::new(file), Arc::clone(&armed), slots);
armed.store(true, Ordering::SeqCst);
// ... spawn a statement that reads through `device` ...
reached_rx.recv().unwrap();
// ... assert some other session's request is still fast here ...
release_tx.send(()).unwrap();
```

`cargo test -p engine --test dispatch_never_blocks` runs this file alone;
it needs the `test-util` feature (enabled automatically for `cargo test`
via this crate's `[dev-dependencies]`) for `Database::open_with_devices`.
Run under `taskset -c 0,1` per `CLAUDE.md`'s concurrency-test discipline,
the same as `tests/sessions.rs`'s concurrent tests.
