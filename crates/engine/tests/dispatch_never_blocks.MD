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
  function itself never reaches its release send.
- `PauseSlot` - one arming's worth of synchronization: the `reached`
  `SyncSender` the device signals when it parks, and the `release`
  `Receiver` it then blocks on. The device holds a `VecDeque` of these and
  consumes one per arming, which is what lets a single test pause two
  different threads in sequence at two different reads.
- `PausingDevice` - wraps a real `FileDevice` and pauses exactly the next
  `read_at` call after `armed` (an `Arc<AtomicBool>`, shared with the
  test so it can arm the device after the database is already open and
  the device has moved into `DiskManager`) is set: it pops the front
  `PauseSlot` (the queue is `Mutex`-wrapped only so `read_at`, which takes
  `&self`, can mutate it), signals that slot's `reached`, and blocks on its
  `release` (bounded by `PAUSE_SAFETY_NET`) before delegating to the
  wrapped device. `armed.swap(false, ...)` ensures only the *next* read
  after each arming pauses, not every read forever after - every other
  `BlockDevice` method delegates straight through unpaused.
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
